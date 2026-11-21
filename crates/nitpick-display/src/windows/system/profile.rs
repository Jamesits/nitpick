//! `mscms.dll` profile calls.
//!
//! All functions are resolved at run time. The `ColorProfile*` functions
//! exist from Windows build 20348; a static import would stop the process
//! from loading on older Windows.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ::windows::Win32::Foundation::{
    E_ACCESSDENIED, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, GetLastError, HLOCAL,
    LUID, LocalFree,
};
use ::windows::Win32::Graphics::Gdi::{DISPLAY_DEVICEW, EnumDisplayDevicesW};
use ::windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use ::windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_MULTI_SZ, RegGetValueW,
};
use ::windows::core::{BOOL, HRESULT, HSTRING, PCSTR, PCWSTR, PWSTR, s, w};
use nitpick_core::device::{DeviceError, ProfileRole, UserScope};

use super::{System, wide};
use crate::common::unsupported;
use crate::windows::{DisplayAddress, WindowsProfileNative};

/// `WCS_PROFILE_MANAGEMENT_SCOPE`.
const SCOPE_SYSTEM_WIDE: i32 = 0;
const SCOPE_CURRENT_USER: i32 = 1;
/// `COLORPROFILETYPE::CPT_ICC`.
const CPT_ICC: i32 = 0;
/// `COLORPROFILESUBTYPE`.
const CPST_STANDARD_DISPLAY_COLOR_MODE: i32 = 7;
const CPST_EXTENDED_DISPLAY_COLOR_MODE: i32 = 8;
/// `CLASS_MONITOR`, the ICC device class `mntr`.
const CLASS_MONITOR: u32 = u32::from_be_bytes(*b"mntr");
/// `EDD_GET_DEVICE_INTERFACE_NAME`.
const EDD_GET_DEVICE_INTERFACE_NAME: u32 = 1;
/// `HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)`.
const HR_FILE_NOT_FOUND: HRESULT = HRESULT(0x8007_0002_u32.cast_signed());

type AddFn = unsafe extern "system" fn(i32, PCWSTR, LUID, u32, BOOL, BOOL) -> HRESULT;
type RemoveFn = unsafe extern "system" fn(i32, PCWSTR, LUID, u32, BOOL) -> HRESULT;
type SetDefaultFn = unsafe extern "system" fn(i32, PCWSTR, i32, i32, LUID, u32) -> HRESULT;
type GetDefaultFn = unsafe extern "system" fn(i32, LUID, u32, i32, i32, *mut PWSTR) -> HRESULT;
type GetUserScopeFn = unsafe extern "system" fn(LUID, u32, *mut i32) -> HRESULT;
type InstallFn = unsafe extern "system" fn(PCWSTR, PCWSTR) -> BOOL;
type UninstallFn = unsafe extern "system" fn(PCWSTR, PCWSTR, BOOL) -> BOOL;
type ColorDirFn = unsafe extern "system" fn(PCWSTR, PWSTR, *mut u32) -> BOOL;
type SetPerUserFn = unsafe extern "system" fn(PCWSTR, u32, BOOL) -> BOOL;

struct Mscms {
    add: AddFn,
    remove: RemoveFn,
    set_default: SetDefaultFn,
    get_default: GetDefaultFn,
    get_user_scope: GetUserScopeFn,
    install: InstallFn,
    uninstall: UninstallFn,
    color_dir: ColorDirFn,
    set_per_user: SetPerUserFn,
}

static MSCMS: OnceLock<Result<Mscms, String>> = OnceLock::new();

/// Resolves `name` as a function of type `F`.
///
/// # Safety
/// `F` must be the `extern "system"` signature of the export `name`.
pub(super) unsafe fn proc<F: Copy>(
    module: ::windows::Win32::Foundation::HMODULE,
    name: PCSTR,
) -> Result<F, String> {
    // SAFETY: `name` is a NUL-terminated literal.
    let f = unsafe { GetProcAddress(module, name) }.ok_or_else(|| {
        // SAFETY: `name` is a NUL-terminated literal.
        format!("mscms.dll has no {}", unsafe { name.display() })
    })?;
    assert_eq!(size_of::<F>(), size_of_val(&f));
    // SAFETY: the caller states that `F` is the export's signature; both
    // are function pointers of the same size.
    Ok(unsafe { std::mem::transmute_copy::<_, F>(&f) })
}

fn load() -> Result<Mscms, String> {
    // SAFETY: loads a system DLL by name from System32 only. The module is
    // never freed, so the function pointers stay valid.
    let module = unsafe { LoadLibraryExW(w!("mscms.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
        .map_err(|e| format!("LoadLibraryExW(mscms.dll): {e}"))?;
    // SAFETY: each type is the `icm.h` signature of the named export.
    unsafe {
        Ok(Mscms {
            add: proc(module, s!("ColorProfileAddDisplayAssociation"))?,
            remove: proc(module, s!("ColorProfileRemoveDisplayAssociation"))?,
            set_default: proc(module, s!("ColorProfileSetDisplayDefaultAssociation"))?,
            get_default: proc(module, s!("ColorProfileGetDisplayDefault"))?,
            get_user_scope: proc(module, s!("ColorProfileGetDisplayUserScope"))?,
            install: proc(module, s!("InstallColorProfileW"))?,
            uninstall: proc(module, s!("UninstallColorProfileW"))?,
            color_dir: proc(module, s!("GetColorDirectoryW"))?,
            set_per_user: proc(module, s!("WcsSetUsePerUserProfiles"))?,
        })
    }
}

fn mscms() -> Result<&'static Mscms, DeviceError> {
    MSCMS
        .get_or_init(load)
        .as_ref()
        .map_err(|reason| unsupported("ICC profile control", reason.clone()))
}

fn hr_error(api: &str, hr: HRESULT) -> DeviceError {
    let message = ::windows::core::Error::from_hresult(hr).message();
    if hr == E_ACCESSDENIED {
        return DeviceError::PermissionDenied(format!("{api}: {message}"));
    }
    DeviceError::Native {
        api: api.into(),
        code: Some(i64::from(hr.0.cast_unsigned())),
        message,
    }
}

fn check(api: &str, hr: HRESULT) -> Result<(), DeviceError> {
    if hr.is_ok() {
        Ok(())
    } else {
        Err(hr_error(api, hr))
    }
}

/// For the `BOOL` functions, which report through `GetLastError`.
pub(super) fn check_bool(api: &str, ok: BOOL) -> Result<(), DeviceError> {
    if ok.as_bool() {
        return Ok(());
    }
    // SAFETY: no preconditions.
    let code = unsafe { GetLastError() };
    if code == ERROR_ACCESS_DENIED {
        return Err(DeviceError::PermissionDenied(format!(
            "{api}: access denied"
        )));
    }
    Err(hr_error(api, code.to_hresult()))
}

fn luid(v: i64) -> LUID {
    LUID {
        LowPart: (v.cast_unsigned() & 0xffff_ffff) as u32,
        HighPart: (v >> 32) as i32,
    }
}

fn scope(s: UserScope) -> i32 {
    match s {
        UserScope::Current => SCOPE_CURRENT_USER,
        UserScope::All => SCOPE_SYSTEM_WIDE,
    }
}

fn subtype(role: ProfileRole) -> i32 {
    match role {
        ProfileRole::Standard => CPST_STANDARD_DISPLAY_COLOR_MODE,
        ProfileRole::AdvancedColor => CPST_EXTENDED_DISPLAY_COLOR_MODE,
    }
}

fn advanced(role: ProfileRole) -> BOOL {
    BOOL::from(role == ProfileRole::AdvancedColor)
}

/// Frees a buffer that an `mscms.dll` function allocated.
fn local_free<T>(p: *mut T) {
    if !p.is_null() {
        // SAFETY: the API reference requires `LocalFree` for these buffers.
        unsafe { LocalFree(Some(HLOCAL(p.cast()))) };
    }
}

/// A file name in the colour directory: no path separators or parent
/// references, so a name from the registry cannot address another file.
fn check_file_name(name: &str) -> Result<(), DeviceError> {
    let bad =
        name.is_empty() || name == "." || name == ".." || name.contains(['\\', '/', ':', '\0']);
    if bad {
        return Err(unsupported(
            "profile file",
            format!("{name:?} is not a plain file name"),
        ));
    }
    Ok(())
}

fn io_error(api: &str, path: &Path, e: &std::io::Error) -> DeviceError {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        return DeviceError::PermissionDenied(format!("{api} {}: {e}", path.display()));
    }
    DeviceError::Native {
        api: api.into(),
        code: e.raw_os_error().map(i64::from),
        message: format!("{}: {e}", path.display()),
    }
}

fn color_directory(m: &Mscms) -> Result<PathBuf, DeviceError> {
    let mut buf = [0u16; 260];
    let mut size = u32::try_from(size_of_val(&buf)).expect("buffer size");
    // SAFETY: `size` is the byte length of `buf`.
    let ok = unsafe { (m.color_dir)(PCWSTR::null(), PWSTR(buf.as_mut_ptr()), &raw mut size) };
    check_bool("GetColorDirectoryW", ok)?;
    Ok(PathBuf::from(wide(&buf)))
}

/// `EnumDisplayDevicesW` names of the monitor: the legacy WCS device name
/// `MONITOR\<id>\{class}\<n>` and its driver key
/// `\Registry\Machine\...\Class\{class}\<n>`. The monitors of a GDI source
/// are listed in order; the one whose interface name is the device path is
/// the display.
fn monitor_names(display: &DisplayAddress) -> Result<(String, String), DeviceError> {
    let gdi = display
        .gdi_name
        .as_deref()
        .ok_or_else(|| unsupported("monitor device name", "the display has no GDI source name"))?;
    let gdi = HSTRING::from(gdi);
    let enumerate = |i: u32, flags: u32| {
        let mut dev = DISPLAY_DEVICEW {
            cb: u32::try_from(size_of::<DISPLAY_DEVICEW>()).expect("struct size"),
            ..Default::default()
        };
        // SAFETY: `dev.cb` is the structure size.
        let ok = unsafe { EnumDisplayDevicesW(&gdi, i, &raw mut dev, flags) };
        ok.as_bool()
            .then(|| (wide(&dev.DeviceID), wide(&dev.DeviceKey)))
    };
    for i in 0.. {
        let Some((interface, _)) = enumerate(i, EDD_GET_DEVICE_INTERFACE_NAME) else {
            break;
        };
        if interface.eq_ignore_ascii_case(&display.device_path) {
            return enumerate(i, 0).ok_or_else(|| DeviceError::Native {
                api: "EnumDisplayDevicesW".into(),
                code: None,
                message: format!("monitor {i} of {gdi} disappeared"),
            });
        }
    }
    Err(unsupported(
        "monitor device name",
        format!("{gdi} lists no monitor {}", display.device_path),
    ))
}

/// `{class}\<n>` of a driver key.
fn driver_instance(key: &str) -> Result<&str, DeviceError> {
    const MARKER: &str = r"\control\class\";
    key.to_ascii_lowercase()
        .find(MARKER)
        .map(|i| &key[i + MARKER.len()..])
        .filter(|rest| rest.matches('\\').count() == 1)
        .ok_or_else(|| unsupported("monitor driver key", format!("unexpected key {key}")))
}

/// A `REG_MULTI_SZ` value; a missing key or value is an empty list.
fn registry_list(root: HKEY, subkey: &str, value: &str) -> Result<Vec<String>, DeviceError> {
    let (subkey_w, value_w) = (HSTRING::from(subkey), HSTRING::from(value));
    let read = |buf: Option<&mut [u16]>, size: &mut u32| {
        let ptr = buf.map(|b| b.as_mut_ptr().cast::<std::ffi::c_void>());
        // SAFETY: `size` is the byte length of `buf`, or the buffer is
        // absent for a size query.
        unsafe {
            RegGetValueW(
                root,
                &subkey_w,
                &value_w,
                RRF_RT_REG_MULTI_SZ,
                None,
                ptr,
                Some(size),
            )
        }
    };
    let api = format!(r"RegGetValueW({subkey}\{value})");
    let mut size = 0u32;
    match read(None, &mut size) {
        e if e == ERROR_FILE_NOT_FOUND => return Ok(Vec::new()),
        e if e != ERROR_SUCCESS => return Err(hr_error(&api, e.to_hresult())),
        _ => {}
    }
    let mut buf = vec![0u16; (size as usize).div_ceil(2)];
    let e = read(Some(&mut buf), &mut size);
    if e != ERROR_SUCCESS {
        return Err(hr_error(&api, e.to_hresult()));
    }
    buf.truncate(size as usize / 2);
    Ok(buf
        .split(|&c| c == 0)
        .filter(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect())
}

/// The association list `value` (`ICMProfile` or `ICMProfileAC`) of the
/// monitor. Current user: `HKCU\...\ICM\ProfileAssociations\Display\{class}\<n>`
/// (both values observed). System-wide: the monitor's driver key in HKLM,
/// where `ICMProfile` of another monitor was observed; `ICMProfileAC` there
/// is unverified.
fn registry_association_list(
    display: &DisplayAddress,
    s: UserScope,
    value: &str,
) -> Result<Vec<String>, DeviceError> {
    let (_, key) = monitor_names(display)?;
    let instance = driver_instance(&key)?;
    let (root, subkey) = match s {
        UserScope::Current => (
            HKEY_CURRENT_USER,
            format!(
                r"Software\Microsoft\Windows NT\CurrentVersion\ICM\ProfileAssociations\Display\{instance}"
            ),
        ),
        UserScope::All => (
            HKEY_LOCAL_MACHINE,
            format!(r"SYSTEM\CurrentControlSet\Control\Class\{instance}"),
        ),
    };
    registry_list(root, &subkey, value)
}

impl WindowsProfileNative for System {
    fn profile_api(&mut self) -> Result<(), DeviceError> {
        mscms().map(|_| ())
    }

    fn applied_scope(&mut self, display: &DisplayAddress) -> Result<UserScope, DeviceError> {
        let m = mscms()?;
        let mut value = -1;
        // SAFETY: `value` is valid for a write.
        let hr = unsafe {
            (m.get_user_scope)(
                luid(display.target_adapter_luid),
                display.source_id,
                &raw mut value,
            )
        };
        check("ColorProfileGetDisplayUserScope", hr)?;
        match value {
            SCOPE_CURRENT_USER => Ok(UserScope::Current),
            SCOPE_SYSTEM_WIDE => Ok(UserScope::All),
            other => Err(DeviceError::Native {
                api: "ColorProfileGetDisplayUserScope".into(),
                code: None,
                message: format!("unknown scope {other}"),
            }),
        }
    }

    fn set_per_user(
        &mut self,
        display: &DisplayAddress,
        per_user: bool,
    ) -> Result<(), DeviceError> {
        let m = mscms()?;
        let name = HSTRING::from(monitor_names(display)?.0);
        // SAFETY: `name` is a NUL-terminated string that outlives the call.
        let ok = unsafe { (m.set_per_user)(PCWSTR(name.as_ptr()), CLASS_MONITOR, per_user.into()) };
        check_bool("WcsSetUsePerUserProfiles", ok)
    }

    fn association_list(
        &mut self,
        display: &DisplayAddress,
        s: UserScope,
        role: ProfileRole,
    ) -> Result<(Vec<String>, &'static str), DeviceError> {
        let (value, source) = match role {
            ProfileRole::Standard => ("ICMProfile", "registry ICMProfile"),
            ProfileRole::AdvancedColor => ("ICMProfileAC", "registry ICMProfileAC"),
        };
        registry_association_list(display, s, value).map(|l| (l, source))
    }

    fn default_profile(
        &mut self,
        display: &DisplayAddress,
        s: UserScope,
        role: ProfileRole,
    ) -> Result<Option<String>, DeviceError> {
        let m = mscms()?;
        let mut name = PWSTR::null();
        // SAFETY: `name` is valid for a write.
        let hr = unsafe {
            (m.get_default)(
                scope(s),
                luid(display.target_adapter_luid),
                display.source_id,
                CPT_ICC,
                subtype(role),
                &raw mut name,
            )
        };
        // Observed: no default gives `ERROR_FILE_NOT_FOUND`.
        if hr == HR_FILE_NOT_FOUND {
            local_free(name.0);
            return Ok(None);
        }
        let result = check("ColorProfileGetDisplayDefault", hr).map(|()| {
            // SAFETY: on success `name` is a NUL-terminated string.
            Some(unsafe { name.to_string() }.unwrap_or_default())
        });
        local_free(name.0);
        result
    }

    fn add_association(
        &mut self,
        display: &DisplayAddress,
        s: UserScope,
        role: ProfileRole,
        name: &str,
    ) -> Result<(), DeviceError> {
        let m = mscms()?;
        let name = HSTRING::from(name);
        // SAFETY: `name` outlives the call.
        let hr = unsafe {
            (m.add)(
                scope(s),
                PCWSTR(name.as_ptr()),
                luid(display.target_adapter_luid),
                display.source_id,
                BOOL(0),
                advanced(role),
            )
        };
        check("ColorProfileAddDisplayAssociation", hr)
    }

    fn remove_association(
        &mut self,
        display: &DisplayAddress,
        s: UserScope,
        role: ProfileRole,
        name: &str,
    ) -> Result<(), DeviceError> {
        let m = mscms()?;
        let name = HSTRING::from(name);
        // SAFETY: `name` outlives the call.
        let hr = unsafe {
            (m.remove)(
                scope(s),
                PCWSTR(name.as_ptr()),
                luid(display.target_adapter_luid),
                display.source_id,
                advanced(role),
            )
        };
        check("ColorProfileRemoveDisplayAssociation", hr)
    }

    fn set_default(
        &mut self,
        display: &DisplayAddress,
        s: UserScope,
        role: ProfileRole,
        name: &str,
    ) -> Result<(), DeviceError> {
        let m = mscms()?;
        let name = HSTRING::from(name);
        // SAFETY: `name` outlives the call.
        let hr = unsafe {
            (m.set_default)(
                scope(s),
                PCWSTR(name.as_ptr()),
                CPT_ICC,
                subtype(role),
                luid(display.target_adapter_luid),
                display.source_id,
            )
        };
        check("ColorProfileSetDisplayDefaultAssociation", hr)
    }

    fn installed_profiles(&mut self) -> Result<Vec<String>, DeviceError> {
        let dir = color_directory(mscms()?)?;
        let entries = std::fs::read_dir(&dir).map_err(|e| io_error("read_dir", &dir, &e))?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| io_error("read_dir", &dir, &e))?;
            let is_file = entry.file_type().is_ok_and(|t| t.is_file());
            if let (true, Some(name)) = (is_file, entry.file_name().to_str()) {
                names.push(name.to_owned());
            }
        }
        Ok(names)
    }

    fn read_installed(&mut self, name: &str) -> Result<Option<Vec<u8>>, DeviceError> {
        check_file_name(name)?;
        let path = color_directory(mscms()?)?.join(name);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_error("read", &path, &e)),
        }
    }

    fn install(&mut self, name: &str, bytes: &[u8]) -> Result<(), DeviceError> {
        check_file_name(name)?;
        let m = mscms()?;
        // `InstallColorProfileW` copies a file and keeps its name, so the
        // source file must have the planned name.
        let dir = std::env::temp_dir().join(format!("nitpick-icc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| io_error("create_dir", &dir, &e))?;
        let source = dir.join(name);
        let result = std::fs::write(&source, bytes)
            .map_err(|e| io_error("write", &source, &e))
            .and_then(|()| {
                let path = HSTRING::from(source.as_os_str());
                // SAFETY: `path` outlives the call.
                let ok = unsafe { (m.install)(PCWSTR::null(), PCWSTR(path.as_ptr())) };
                check_bool("InstallColorProfileW", ok)
            });
        let _ = std::fs::remove_file(&source);
        let _ = std::fs::remove_dir(&dir);
        result
    }

    fn uninstall(&mut self, name: &str) -> Result<(), DeviceError> {
        check_file_name(name)?;
        let m = mscms()?;
        let name = HSTRING::from(name);
        // SAFETY: `name` outlives the call.
        let ok = unsafe { (m.uninstall)(PCWSTR::null(), PCWSTR(name.as_ptr()), BOOL(1)) };
        check_bool("UninstallColorProfileW", ok)
    }
}
