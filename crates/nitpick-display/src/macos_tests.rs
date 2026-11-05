use std::cell::RefCell;
use std::rc::Rc;

use nitpick_core::device::{
    DeviceError, DeviceId, DeviceQuery, DisplayDeviceControl, EdrHeadroom, HostScope, ProfileRole,
    ProfileScope, ProfileTarget, Report, UserScope,
};
use nitpick_core::driver::Support;

use super::{
    ColorSyncDevice, DisplayReport, MODE_NATIVE_FLAG, MacDisplay, MacNative, ModeReport,
    ScreenReport,
};
use crate::common::NativeFailure;

const ILLEGAL_ARGUMENT: i64 = 1001; // kCGErrorIllegalArgument

fn fail(api: &'static str) -> NativeFailure {
    NativeFailure {
        api,
        code: ILLEGAL_ARGUMENT,
    }
}

#[derive(Default)]
struct State {
    displays: Option<Vec<DisplayReport>>,
    screens: Option<Result<Vec<ScreenReport>, String>>,
}

#[derive(Clone, Default)]
struct Fake(Rc<RefCell<State>>);

impl MacNative for Fake {
    fn displays(&mut self) -> Result<Vec<DisplayReport>, DeviceError> {
        self.0
            .borrow()
            .displays
            .clone()
            .ok_or_else(|| fail("CGGetActiveDisplayList").into())
    }

    fn screens(&mut self) -> Result<Vec<ScreenReport>, String> {
        self.0.borrow().screens.clone().unwrap_or(Ok(Vec::new()))
    }
}

impl Fake {
    fn set(&self, displays: Vec<DisplayReport>, screens: Vec<ScreenReport>) {
        let mut s = self.0.borrow_mut();
        s.displays = Some(displays);
        s.screens = Some(Ok(screens));
    }

    fn backend(&self) -> MacDisplay<Self> {
        MacDisplay::with_native(self.clone())
    }
}

fn mode(w: usize, h: usize, io_flags: u32) -> ModeReport {
    ModeReport {
        pixel_width: w,
        pixel_height: h,
        io_flags,
    }
}

/// An external 4K monitor with a registered `ColorSync` device. The scaled
/// `HiDPI` modes have other pixel sizes; only one size has the native flag.
fn display(n: u32) -> DisplayReport {
    DisplayReport {
        display_id: n,
        uuid: Ok(format!("00000000-0000-0000-0000-00000000000{n}")),
        vendor: 0x10ac,
        model: 0x4321,
        serial: 0x1000 + n,
        builtin: false,
        modes: Ok(vec![
            mode(5120, 2880, 0x3),
            mode(3840, 2160, MODE_NATIVE_FLAG | 0x3),
            mode(3840, 2160, MODE_NATIVE_FLAG | 0x7),
            mode(1920, 1080, 0x3),
        ]),
        colorsync: Some(ColorSyncDevice {
            description: Some("ColorSync name".into()),
            default_profile_id: Some("1".into()),
        }),
    }
}

/// EDR on: headroom 2 now, 16 available, no reference preset.
fn screen(n: u32, name: &str) -> ScreenReport {
    ScreenReport {
        display_id: n,
        name: name.into(),
        edr_current: 2.0,
        edr_potential: 16.0,
        edr_reference: 0.0,
        p3: true,
    }
}

fn query_one(display: DisplayReport, screens: Vec<ScreenReport>) -> DeviceQuery {
    let fake = Fake::default();
    fake.set(vec![display], screens);
    let mut b = fake.backend();
    let id = b.devices().unwrap().remove(0).id;
    b.query_device(&id).unwrap()
}

fn value<T: Clone>(r: &Report<T>) -> Option<T> {
    r.value().cloned()
}

fn source<T>(r: &Report<T>) -> &str {
    match r {
        Report::Known { source, .. } => source,
        Report::Unknown(reason) => panic!("unknown: {reason}"),
    }
}

fn reason<T: std::fmt::Debug>(r: &Report<T>) -> &str {
    match r {
        Report::Unknown(reason) => reason,
        known @ Report::Known { .. } => panic!("known: {known:?}"),
    }
}

#[test]
fn readable_fields() {
    let q = query_one(display(1), vec![screen(1, "Monitor")]);
    assert_eq!(q.device.id.backend, "macos");
    assert_eq!(q.device.id.key, "00000000-0000-0000-0000-000000000001");
    assert_eq!(q.device.name, "Monitor");
    assert_eq!(
        q.device.evidence,
        vec![
            ("vendor_model".into(), "000010AC:00004321".into()),
            ("serial".into(), "00001001".into()),
            ("builtin".into(), "false".into()),
        ]
    );
    assert_eq!(q.capabilities.sdr_wcg.support(), Support::Yes);
    assert_eq!(
        source(&q.capabilities.sdr_wcg),
        "NSScreen.canRepresentDisplayGamut"
    );
    assert_eq!(
        value(&q.mode.edr),
        Some(EdrHeadroom {
            current: 2.0,
            potential: 16.0,
            reference: None,
        })
    );
    assert_eq!(value(&q.mode.native_size), Some((3840, 2160)));
    assert_eq!(source(&q.mode.native_size), "CGDisplayCopyAllDisplayModes");
    assert!(reason(&q.capabilities.icc_profiles).contains("not implemented"));
}

/// EDR headroom is a ratio to an SDR white of unknown luminance. It must
/// not fill a luminance field or report HDR.
#[test]
fn edr_headroom_is_not_luminance_or_hdr() {
    let mut s = screen(1, "Monitor");
    s.edr_reference = 4.0;
    let q = query_one(display(1), vec![s]);
    assert_eq!(value(&q.mode.edr).unwrap().reference, Some(4.0));
    let m = &q.mode;
    for r in [
        &m.peak_nits,
        &m.min_nits,
        &m.full_frame_nits,
        &m.sdr_white_nits,
    ] {
        assert!(reason(r).contains("not luminance"));
    }
    assert!(matches!(q.capabilities.hdr.support(), Support::Unknown(_)));
    assert!(reason(&m.hdr_on).contains("EDR headroom does not establish HDR"));
}

/// P3 support is a capability; the active signal gamut stays unknown.
#[test]
fn supported_and_active_state_stay_separate() {
    let q = query_one(display(1), vec![screen(1, "Monitor")]);
    assert_eq!(q.capabilities.sdr_wcg.support(), Support::Yes);
    assert!(reason(&q.mode.gamut).contains("signal gamut"));
    assert!(reason(&q.capabilities.wire_depths).contains("wire depth"));
    assert!(reason(&q.mode.wire_depth).contains("wire depth"));

    let mut s = screen(1, "Monitor");
    s.p3 = false;
    let q = query_one(display(1), vec![s]);
    assert!(matches!(q.capabilities.sdr_wcg.support(), Support::No(_)));
}

#[test]
fn edr_without_headroom_and_invalid_values() {
    let mut s = screen(1, "Monitor");
    s.edr_current = 1.0;
    s.edr_potential = 1.0;
    let q = query_one(display(1), vec![s.clone()]);
    assert_eq!(
        value(&q.mode.edr),
        Some(EdrHeadroom {
            current: 1.0,
            potential: 1.0,
            reference: None,
        })
    );

    for (current, potential) in [(0.0, 16.0), (2.0, f64::NAN), (f64::INFINITY, 16.0)] {
        s.edr_current = current;
        s.edr_potential = potential;
        let q = query_one(display(1), vec![s.clone()]);
        assert!(reason(&q.mode.edr).contains("NSScreen EDR"));
    }
}

/// The main thread did not answer: the `AppKit` fields are unknown with
/// the reason, and the Core Graphics and `ColorSync` fields remain.
#[test]
fn appkit_failure_is_partial() {
    let fake = Fake::default();
    fake.set(vec![display(1)], vec![]);
    fake.0.borrow_mut().screens = Some(Err("main thread busy".into()));
    let mut b = fake.backend();
    let info = b.devices().unwrap().remove(0);
    assert_eq!(info.name, "ColorSync name");
    let q = b.query_device(&info.id).unwrap();
    assert_eq!(reason(&q.capabilities.sdr_wcg), "main thread busy");
    assert_eq!(reason(&q.mode.edr), "main thread busy");
    assert_eq!(value(&q.mode.native_size), Some((3840, 2160)));

    // A screen list without this display.
    let q = query_one(display(1), vec![screen(2, "Other")]);
    assert_eq!(q.device.name, "ColorSync name");
    assert_eq!(reason(&q.mode.edr), "no NSScreen for display 1");
}

#[test]
fn every_failed_field_is_unknown_with_reason() {
    let d = DisplayReport {
        display_id: 7,
        uuid: Err(fail("CGDisplayCreateUUIDFromDisplayID")),
        vendor: 0,
        model: 0,
        serial: 0,
        builtin: false,
        modes: Err(fail("CGDisplayCopyAllDisplayModes")),
        colorsync: None,
    };
    let fake = Fake::default();
    fake.set(vec![d], vec![]);
    fake.0.borrow_mut().screens = Some(Err("main thread busy".into()));
    let mut b = fake.backend();
    let info = b.devices().unwrap().remove(0);
    // The fallback key is the display ID.
    assert_eq!(info.id.key, "display:7");
    assert_eq!(info.name, "Display 7");
    let q = b.query_device(&info.id).unwrap();
    assert!(reason(&q.capabilities.icc_profiles).contains("CGDisplayCreateUUIDFromDisplayID"));
    assert!(reason(&q.mode.native_size).starts_with("CGDisplayCopyAllDisplayModes failed"));
    assert_eq!(reason(&q.mode.edr), "main thread busy");
}

#[test]
fn unregistered_colorsync_device_has_no_profile_control() {
    let mut d = display(1);
    d.colorsync = None;
    let q = query_one(d, vec![screen(1, "Monitor")]);
    assert!(matches!(
        q.capabilities.icc_profiles.support(),
        Support::No(reason) if reason.contains("ColorSyncDeviceCopyDeviceInfo")
    ));
}

#[test]
fn native_mode_must_be_unique() {
    let mut d = display(1);
    d.modes = Ok(vec![mode(3840, 2160, 0x3), mode(1920, 1080, 0x3)]);
    let q = query_one(d.clone(), vec![]);
    assert!(reason(&q.mode.native_size).contains("no mode has the native flag"));

    d.modes = Ok(vec![
        mode(3840, 2160, MODE_NATIVE_FLAG),
        mode(2560, 1440, MODE_NATIVE_FLAG),
    ]);
    let q = query_one(d.clone(), vec![]);
    assert!(reason(&q.mode.native_size).contains("2 native mode sizes"));

    d.modes = Ok(vec![mode(0, 0, MODE_NATIVE_FLAG)]);
    let q = query_one(d, vec![]);
    assert!(reason(&q.mode.native_size).contains("no mode has the native flag"));
}

#[test]
fn duplicate_names_have_distinct_devices() {
    let fake = Fake::default();
    let mut second = screen(2, "Monitor");
    second.edr_potential = 1.0;
    second.edr_current = 1.0;
    fake.set(
        vec![display(1), display(2)],
        vec![screen(1, "Monitor"), second],
    );
    let mut b = fake.backend();
    let devices = b.devices().unwrap();
    assert_eq!(devices[0].name, devices[1].name);
    assert_ne!(devices[0].id, devices[1].id);
    let first = b.query_device(&devices[0].id).unwrap();
    let second = b.query_device(&devices[1].id).unwrap();
    assert_eq!(value(&first.mode.edr).map(|e| e.potential), Some(16.0));
    assert_eq!(value(&second.mode.edr).map(|e| e.potential), Some(1.0));
}

/// Two identical monitors without serial numbers can share a UUID.
#[test]
fn shared_uuid_is_ambiguous() {
    let fake = Fake::default();
    let mut twin = display(2);
    twin.uuid = display(1).uuid;
    fake.set(vec![display(1), twin], vec![]);
    let mut b = fake.backend();
    let devices = b.devices().unwrap();
    assert_eq!(devices.len(), 2);
    assert!(matches!(
        b.query_device(&devices[0].id),
        Err(DeviceError::AmbiguousIdentity(_))
    ));
    assert!(matches!(
        b.profile_targets(&devices[0].id),
        Err(DeviceError::AmbiguousIdentity(_))
    ));
}

#[test]
fn identity_changes() {
    let fake = Fake::default();
    fake.set(
        vec![display(1), display(2)],
        vec![screen(1, "A"), screen(2, "B")],
    );
    let mut b = fake.backend();
    let devices = b.devices().unwrap();
    let rev =
        |b: &mut MacDisplay<Fake>, id: &DeviceId| b.query_device(id).unwrap().topology_revision;
    let base = rev(&mut b, &devices[0].id);

    // Reconnect: new display IDs and order, new screen names.
    let mut moved = vec![display(2), display(1)];
    for d in &mut moved {
        d.display_id += 10;
    }
    fake.set(moved, vec![screen(11, "A2"), screen(12, "B2")]);
    assert_eq!(rev(&mut b, &devices[0].id), base);
    assert_eq!(b.query_device(&devices[0].id).unwrap().device.name, "A2");

    // Same UUID, other evidence: the topology revision changes.
    let mut changed = display(2);
    changed.serial = 0x9999;
    fake.set(vec![display(1), changed], vec![]);
    assert_ne!(rev(&mut b, &devices[0].id), base);

    // Monitor 2 is replaced: a new UUID.
    let mut replaced = display(2);
    replaced.uuid = Ok("FFFFFFFF-0000-0000-0000-000000000002".into());
    fake.set(vec![display(1), replaced], vec![]);
    assert_ne!(rev(&mut b, &devices[0].id), base);
    assert_eq!(
        b.query_device(&devices[1].id).unwrap_err(),
        DeviceError::Disconnected(devices[1].id.clone())
    );

    // Monitor 2 is disconnected.
    fake.set(vec![display(1)], vec![]);
    assert_eq!(b.devices().unwrap().len(), 1);
    assert!(matches!(
        b.query_device(&devices[1].id),
        Err(DeviceError::Disconnected(_))
    ));
}

#[test]
fn display_list_failure_is_an_error() {
    let mut b = Fake::default().backend();
    assert!(matches!(
        b.devices(),
        Err(DeviceError::Native { api, code: Some(ILLEGAL_ARGUMENT), .. })
            if api == "CGGetActiveDisplayList"
    ));
}

#[test]
fn profile_control_is_explicitly_unsupported() {
    let fake = Fake::default();
    fake.set(vec![display(1)], vec![]);
    let mut b = fake.backend();
    let id = b.devices().unwrap().remove(0).id;
    assert_eq!(b.profile_targets(&id).unwrap(), vec![]);
    let target = ProfileTarget {
        device: id,
        role: ProfileRole::Standard,
        scope: ProfileScope {
            user: UserScope::Current,
            host: Some(HostScope::Current),
        },
    };
    assert!(matches!(
        b.profile_state(&target),
        Err(DeviceError::Unsupported { reason, .. }) if reason.contains("not implemented")
    ));
}
