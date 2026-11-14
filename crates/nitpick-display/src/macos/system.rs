//! Core Graphics, `ColorSync` and `AppKit` calls.
//!
//! Core Graphics display queries and `ColorSync` device queries run on the
//! calling worker. `NSScreen` is main-thread only: a call on another thread
//! is sent to the main dispatch queue and waits a bounded time, because a
//! host without a main run loop never drains that queue.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::Duration;

use dispatch2::DispatchQueue;
use nitpick_core::device::{DeviceError, HostScope, UserScope};
use objc2::MainThreadMarker;
use objc2::rc::autoreleasepool;
use objc2_app_kit::{NSDisplayGamut, NSScreen};
use objc2_color_sync::{
    ColorSyncDeviceCopyDeviceInfo, ColorSyncDeviceSetCustomProfiles, ColorSyncProfile,
    kColorSyncCustomProfiles, kColorSyncDeviceDefaultProfileID, kColorSyncDeviceDescription,
    kColorSyncDeviceHostScope, kColorSyncDeviceUserScope, kColorSyncDisplayDeviceClass,
    kColorSyncFactoryProfiles, kColorSyncProfileHostScope, kColorSyncProfileUserScope,
};
use objc2_core_foundation::{
    CFBoolean, CFData, CFDictionary, CFError, CFNumber, CFPreferencesCopyValue,
    CFPreferencesSynchronize, CFRetained, CFString, CFType, CFURL, CFUUID, kCFNull,
    kCFPreferencesAnyApplication, kCFPreferencesAnyHost, kCFPreferencesAnyUser,
    kCFPreferencesCurrentHost, kCFPreferencesCurrentUser,
};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayCopyAllDisplayModes, CGDisplayIsBuiltin, CGDisplayMode,
    CGDisplayModelNumber, CGDisplaySerialNumber, CGDisplayVendorNumber, CGError,
    CGGetActiveDisplayList, kCGDisplayShowDuplicateLowResolutionModes,
};
use objc2_foundation::{NSNumber, ns_string};

use super::{
    ColorSyncDevice, DisplayReport, MacNative, MacProfileNative, MacScope, ModeReport, ScreenReport,
};
use crate::common::{Field, NativeFailure};

/// Global-domain key of the per-device `ColorSync` settings. Undocumented.
const DEVICES_KEY: &str = "com.apple.ColorSync.Devices";
/// Key of the custom profile dictionary in a device entry. Undocumented;
/// equal to the `kColorSyncCustomProfiles` value in observed dumps.
const CUSTOM_PROFILES_KEY: &str = "CustomProfiles";

/// Bound on the wait for the main thread, for a busy GUI thread. Not
/// measured.
const MAIN_THREAD_WAIT: Duration = Duration::from_secs(2);

#[link(name = "ColorSync", kind = "framework")]
unsafe extern "C-unwind" {
    // Declared here: the binding in `objc2-color-sync` panics on NULL.
    fn CGDisplayCreateUUIDFromDisplayID(display: CGDirectDisplayID) -> Option<NonNull<CFUUID>>;
}

pub struct System;

fn cg(api: &'static str, e: CGError) -> NativeFailure {
    NativeFailure {
        api,
        code: i64::from(e.0),
    }
}

fn active_displays() -> Result<Vec<CGDirectDisplayID>, DeviceError> {
    let mut count = 0u32;
    // SAFETY: a null list with 0 entries returns the count only.
    let e = unsafe { CGGetActiveDisplayList(0, std::ptr::null_mut(), &raw mut count) };
    if e != CGError::Success {
        return Err(cg("CGGetActiveDisplayList", e).into());
    }
    let mut ids = vec![0; count as usize];
    // SAFETY: the list has `count` entries. A display added after the first
    // call is left out until the next query.
    let e = unsafe { CGGetActiveDisplayList(count, ids.as_mut_ptr(), &raw mut count) };
    if e != CGError::Success {
        return Err(cg("CGGetActiveDisplayList", e).into());
    }
    ids.truncate(count as usize);
    Ok(ids)
}

fn uuid(display: CGDirectDisplayID) -> Field<CFRetained<CFUUID>> {
    // SAFETY: any display ID is valid input; an unknown ID gives NULL.
    let uuid = unsafe { CGDisplayCreateUUIDFromDisplayID(display) };
    uuid.map(|p| {
        // SAFETY: a "Create" function returns a +1 reference.
        unsafe { CFRetained::from_raw(p) }
    })
    .ok_or(NativeFailure {
        api: "CGDisplayCreateUUIDFromDisplayID",
        code: 0,
    })
}

fn modes(display: CGDirectDisplayID) -> Field<Vec<ModeReport>> {
    let key: &CFString = unsafe { kCGDisplayShowDuplicateLowResolutionModes };
    let options = CFDictionary::<CFString, CFBoolean>::from_slices(&[key], &[CFBoolean::new(true)]);
    // SAFETY: the option value is a CFBoolean, as documented.
    let list = unsafe { CGDisplayCopyAllDisplayModes(display, Some(options.as_opaque())) }.ok_or(
        NativeFailure {
            api: "CGDisplayCopyAllDisplayModes",
            code: 0,
        },
    )?;
    // SAFETY: the array holds `CGDisplayMode` values.
    let list = unsafe { list.cast_unchecked::<CGDisplayMode>() };
    Ok(list
        .iter()
        .map(|m| ModeReport {
            pixel_width: CGDisplayMode::pixel_width(Some(&m)),
            pixel_height: CGDisplayMode::pixel_height(Some(&m)),
            io_flags: CGDisplayMode::io_flags(Some(&m)),
        })
        .collect())
}

fn text(value: &CFType) -> Option<String> {
    if let Some(s) = value.downcast_ref::<CFString>() {
        return Some(s.to_string());
    }
    value
        .downcast_ref::<CFNumber>()
        .and_then(CFNumber::as_i64)
        .map(|n| n.to_string())
}

/// A dictionary with string keys and property-list or CF values.
fn string_dict(value: CFRetained<CFType>) -> Option<CFRetained<CFDictionary<CFString, CFType>>> {
    let dict = value.downcast::<CFDictionary>().ok()?;
    // SAFETY: ColorSync device info and preference dictionaries have
    // string keys; values are checked by downcasts.
    Some(unsafe { CFRetained::cast_unchecked(dict) })
}

/// A file URL or a path string as a path; other URLs as their text.
fn profile_path(value: &CFType) -> Option<String> {
    if let Some(url) = value.downcast_ref::<CFURL>() {
        return Some(
            url.to_file_path()
                .map_or_else(|| url.string().to_string(), |p| p.display().to_string()),
        );
    }
    let s = value.downcast_ref::<CFString>()?.to_string();
    match CFURL::from_string(None, &CFString::from_str(&s), None) {
        Some(url) if s.starts_with("file:") => url.to_file_path().map(|p| p.display().to_string()),
        _ => Some(s),
    }
}

fn user_scope(value: &CFType) -> Option<UserScope> {
    let s = value.downcast_ref::<CFString>()?;
    // SAFETY: CFPreferences constants.
    let (current, any) = unsafe { (kCFPreferencesCurrentUser, kCFPreferencesAnyUser) };
    if s == current {
        Some(UserScope::Current)
    } else if s == any {
        Some(UserScope::All)
    } else {
        None
    }
}

fn host_scope(value: &CFType) -> Option<HostScope> {
    let s = value.downcast_ref::<CFString>()?;
    // SAFETY: CFPreferences constants.
    let (current, any) = unsafe { (kCFPreferencesCurrentHost, kCFPreferencesAnyHost) };
    if s == current {
        Some(HostScope::Current)
    } else if s == any {
        Some(HostScope::Any)
    } else {
        None
    }
}

fn colorsync(uuid: &CFUUID) -> Option<ColorSyncDevice> {
    // SAFETY: the class is a ColorSync constant and the UUID is valid.
    let info = unsafe { ColorSyncDeviceCopyDeviceInfo(kColorSyncDisplayDeviceClass, uuid) }?;
    let info = string_dict(info.into())?;
    // SAFETY: ColorSync key constants.
    let (description, factory, default_id, custom, user_key, host_key) = unsafe {
        (
            kColorSyncDeviceDescription,
            kColorSyncFactoryProfiles,
            kColorSyncDeviceDefaultProfileID,
            kColorSyncCustomProfiles,
            kColorSyncDeviceUserScope,
            kColorSyncDeviceHostScope,
        )
    };
    // The default profile ID is a key of the factory profile dictionary,
    // next to the profile IDs.
    let factory = info.get(factory).and_then(string_dict);
    let default_profile_id = factory
        .as_ref()
        .and_then(|f| f.get(default_id).as_deref().and_then(text));
    let mut factory_profile_ids: Vec<String> = factory
        .map(|f| {
            let (keys, _) = f.to_vecs();
            keys.iter()
                .filter(|k| ***k != *default_id)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();
    factory_profile_ids.sort_unstable();
    let mut custom_profiles: Vec<(String, Option<String>)> = info
        .get(custom)
        .and_then(string_dict)
        .map(|c| {
            let (keys, values) = c.to_vecs();
            keys.iter()
                .zip(values)
                .map(|(k, v)| (k.to_string(), profile_path(&v)))
                .collect()
        })
        .unwrap_or_default();
    custom_profiles.sort_unstable();
    Some(ColorSyncDevice {
        description: info.get(description).as_deref().and_then(text),
        default_profile_id,
        factory_profile_ids,
        custom_profiles,
        user_scope: info.get(user_key).as_deref().and_then(user_scope),
        host_scope: info.get(host_key).as_deref().and_then(host_scope),
    })
}

fn display_report(display: CGDirectDisplayID) -> DisplayReport {
    let uuid = uuid(display);
    let colorsync = uuid.as_ref().ok().and_then(|u| colorsync(u));
    DisplayReport {
        display_id: display,
        uuid: uuid.map(|u| {
            CFUUID::new_string(None, Some(&u))
                .map(|s| s.to_string())
                .unwrap_or_default()
        }),
        vendor: CGDisplayVendorNumber(display),
        model: CGDisplayModelNumber(display),
        serial: CGDisplaySerialNumber(display),
        builtin: CGDisplayIsBuiltin(display),
        modes: modes(display),
        colorsync,
    }
}

fn read_screens(mtm: MainThreadMarker) -> Vec<ScreenReport> {
    autoreleasepool(|_| {
        NSScreen::screens(mtm)
            .to_vec()
            .into_iter()
            .filter_map(|s| {
                let number = s
                    .deviceDescription()
                    .objectForKey(ns_string!("NSScreenNumber"))?
                    .downcast::<NSNumber>()
                    .ok()?;
                Some(ScreenReport {
                    display_id: number.unsignedIntValue(),
                    name: s.localizedName().to_string(),
                    edr_current: s.maximumExtendedDynamicRangeColorComponentValue(),
                    edr_potential: s.maximumPotentialExtendedDynamicRangeColorComponentValue(),
                    edr_reference: s.maximumReferenceExtendedDynamicRangeColorComponentValue(),
                    p3: s.canRepresentDisplayGamut(NSDisplayGamut::P3),
                })
            })
            .collect()
    })
}

impl MacNative for System {
    fn displays(&mut self) -> Result<Vec<DisplayReport>, DeviceError> {
        Ok(active_displays()?
            .into_iter()
            .map(|d| autoreleasepool(|_| display_report(d)))
            .collect())
    }

    fn screens(&mut self) -> Result<Vec<ScreenReport>, String> {
        if let Some(mtm) = MainThreadMarker::new() {
            return Ok(read_screens(mtm));
        }
        // The block owns the sender, so a late block after a timeout sends
        // to a closed channel and has no other effect.
        let (tx, rx) = mpsc::sync_channel(1);
        DispatchQueue::main().exec_async(move || {
            // After `dispatch_main` the main queue runs on another thread.
            let screens = MainThreadMarker::new()
                .map(read_screens)
                .ok_or_else(|| "the main dispatch queue is not on the main thread".to_owned());
            let _ = tx.send(screens);
        });
        rx.recv_timeout(MAIN_THREAD_WAIT).unwrap_or_else(|_| {
            Err(format!(
                "the main thread did not run the NSScreen query within {} s",
                MAIN_THREAD_WAIT.as_secs()
            ))
        })
    }
}

fn cf_uuid(uuid: &str) -> Result<CFRetained<CFUUID>, DeviceError> {
    CFUUID::from_string(None, Some(&CFString::from_str(uuid))).ok_or_else(|| {
        DeviceError::Unsupported {
            operation: "profile target".into(),
            reason: format!("{uuid} is not a ColorSync device UUID"),
        }
    })
}

fn preference_scope(scope: MacScope) -> (&'static CFString, &'static CFString) {
    // SAFETY: CFPreferences constants.
    unsafe {
        (
            match scope.user {
                UserScope::Current => kCFPreferencesCurrentUser,
                UserScope::All => kCFPreferencesAnyUser,
            },
            match scope.host {
                HostScope::Current => kCFPreferencesCurrentHost,
                HostScope::Any => kCFPreferencesAnyHost,
            },
        )
    }
}

fn unexpected(what: &str) -> DeviceError {
    DeviceError::Native {
        api: "CFPreferencesCopyValue".into(),
        code: None,
        message: format!("{what} has an unexpected type"),
    }
}

fn io_error(api: &str, path: &Path, e: &std::io::Error) -> DeviceError {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => {
            DeviceError::PermissionDenied(format!("{api} {}: {e}", path.display()))
        }
        std::io::ErrorKind::AlreadyExists => {
            DeviceError::StateConflict(format!("{} exists", path.display()))
        }
        _ => DeviceError::Native {
            api: api.into(),
            code: e.raw_os_error().map(i64::from),
            message: format!("{}: {e}", path.display()),
        },
    }
}

/// Takes ownership of an error out-parameter.
fn take_error(e: *mut CFError) -> Option<String> {
    let e = NonNull::new(e)?;
    // SAFETY: ColorSync returns a +1 reference in an error out-parameter.
    let e = unsafe { CFRetained::from_raw(e) };
    Some(
        e.description()
            .map_or_else(|| format!("CFError {}", e.code()), |d| d.to_string()),
    )
}

impl MacProfileNative for System {
    fn device_info(&mut self, uuid: &str) -> Result<Option<ColorSyncDevice>, DeviceError> {
        let id = cf_uuid(uuid)?;
        Ok(autoreleasepool(|_| colorsync(&id)))
    }

    fn scoped_custom_profiles(
        &mut self,
        uuid: &str,
        scope: MacScope,
    ) -> Result<Vec<(String, String)>, DeviceError> {
        let (user, host) = preference_scope(scope);
        // SAFETY: CFPreferences constant.
        let app = unsafe { kCFPreferencesAnyApplication };
        // Reloads values that ColorSync wrote. The result reports only a
        // failed write of pending changes, and this process has none.
        let _ = CFPreferencesSynchronize(app, user, host);
        let Some(devices) =
            CFPreferencesCopyValue(&CFString::from_static_str(DEVICES_KEY), app, user, host)
        else {
            return Ok(Vec::new());
        };
        let devices = string_dict(devices).ok_or_else(|| unexpected(DEVICES_KEY))?;
        // SAFETY: ColorSync constant.
        let class = unsafe { kColorSyncDisplayDeviceClass };
        // Unverified: the case of the UUID in the key. `CFUUIDCreateString`
        // gives upper case, as in the reported dumps.
        let key = CFString::from_str(&format!("Device.{class}.{uuid}"));
        let Some(device) = devices.get(&key) else {
            return Ok(Vec::new());
        };
        let device = string_dict(device).ok_or_else(|| unexpected("device entry"))?;
        let Some(custom) = device.get(&CFString::from_static_str(CUSTOM_PROFILES_KEY)) else {
            return Ok(Vec::new());
        };
        let custom = string_dict(custom).ok_or_else(|| unexpected(CUSTOM_PROFILES_KEY))?;
        let (keys, values) = custom.to_vecs();
        let mut out = Vec::with_capacity(keys.len());
        for (k, v) in keys.iter().zip(values) {
            let path = profile_path(&v).ok_or_else(|| unexpected("custom profile value"))?;
            out.push((k.to_string(), path));
        }
        out.sort_unstable();
        Ok(out)
    }

    fn set_custom_profile(
        &mut self,
        uuid: &str,
        scope: MacScope,
        profile_id: &str,
        path: Option<&str>,
    ) -> Result<(), DeviceError> {
        let native = |message: String| DeviceError::Native {
            api: "ColorSyncDeviceSetCustomProfiles".into(),
            code: None,
            message,
        };
        let id = cf_uuid(uuid)?;
        let (user, host) = preference_scope(scope);
        // SAFETY: ColorSync and CoreFoundation constants.
        let (class, user_key, host_key, null) = unsafe {
            (
                kColorSyncDisplayDeviceClass,
                kColorSyncProfileUserScope,
                kColorSyncProfileHostScope,
                kCFNull,
            )
        };
        let url = match path {
            Some(p) => Some(
                CFURL::from_file_path(p).ok_or_else(|| native(format!("no file URL for {p}")))?,
            ),
            None => None,
        };
        let value: &CFType = match (&url, null) {
            (Some(url), _) => url,
            (None, Some(null)) => null,
            (None, None) => return Err(native("kCFNull is not available".into())),
        };
        let id_key = CFString::from_str(profile_id);
        let keys: [&CFString; 3] = [&id_key, user_key, host_key];
        let values: [&CFType; 3] = [value, user, host];
        let info = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
        // SAFETY: the profile ID maps to a URL or kCFNull, and the scope
        // keys map to CFPreferences constants, as `ColorSyncDevice.h`
        // states.
        let ok = autoreleasepool(|_| unsafe {
            ColorSyncDeviceSetCustomProfiles(class, &id, info.as_opaque())
        });
        if ok {
            Ok(())
        } else {
            Err(native("returned false".into()))
        }
    }

    fn verify_profile(&mut self, bytes: &[u8]) -> Result<(), DeviceError> {
        let data = CFData::from_bytes(bytes);
        let mut error: *mut CFError = std::ptr::null_mut();
        // SAFETY: the out-parameter is valid and null.
        let Some(profile) = (unsafe { ColorSyncProfile::new(&data, &raw mut error) }) else {
            let reason = take_error(error).unwrap_or_else(|| "no reason".into());
            return Err(DeviceError::InvalidProfile(format!(
                "ColorSyncProfileCreate: {reason}"
            )));
        };
        let mut errors: *mut CFError = std::ptr::null_mut();
        let mut warnings: *mut CFError = std::ptr::null_mut();
        // SAFETY: the out-parameters are valid and null.
        let ok = unsafe { profile.verify(&raw mut errors, &raw mut warnings) };
        let errors = take_error(errors);
        drop(take_error(warnings));
        if ok {
            Ok(())
        } else {
            Err(DeviceError::InvalidProfile(format!(
                "ColorSyncProfileVerify: {}",
                errors.unwrap_or_else(|| "no reason".into())
            )))
        }
    }

    fn is_root(&mut self) -> bool {
        rustix::process::geteuid().is_root()
    }

    fn profile_dir(&mut self, user: UserScope) -> Result<String, DeviceError> {
        let base = match user {
            UserScope::Current => std::env::home_dir().ok_or_else(|| DeviceError::Native {
                api: "home_dir".into(),
                code: None,
                message: "no home directory".into(),
            })?,
            UserScope::All => PathBuf::from("/"),
        };
        Ok(base
            .join("Library/ColorSync/Profiles")
            .display()
            .to_string())
    }

    fn read_file(&mut self, path: &str) -> Result<Option<Vec<u8>>, DeviceError> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_error("read", Path::new(path), &e)),
        }
    }

    fn create_file(&mut self, path: &str, bytes: &[u8]) -> Result<(), DeviceError> {
        let path = Path::new(path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io_error("mkdir", dir, &e))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| io_error("open", path, &e))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| io_error("write", path, &e))
    }

    fn remove_file(&mut self, path: &str) -> Result<(), DeviceError> {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(io_error("unlink", Path::new(path), &e))
            }
            _ => Ok(()),
        }
    }
}
