use std::cell::RefCell;
use std::rc::Rc;

use nitpick_core::device::{
    DeviceError, DeviceId, DeviceQuery, DisplayDeviceControl, ProfileRole, ProfileScope,
    ProfileTarget, Report, UserScope,
};
use nitpick_core::driver::{Gamut, Support};

use super::{
    AdvancedColorInfo, AdvancedColorInfo2, DxgiOutput, OutputDesc1, PathReport, TargetName,
    WindowsDisplay, WindowsNative, color_mode, dxgi_color_space, info_2_flags, info_flags,
};
use crate::common::{Field, NativeFailure};

const NOT_SUPPORTED: i64 = 0x32; // ERROR_NOT_SUPPORTED

fn fail(api: &'static str) -> NativeFailure {
    NativeFailure {
        api,
        code: NOT_SUPPORTED,
    }
}

#[derive(Default)]
struct State {
    paths: Option<Vec<PathReport>>,
    dxgi: Option<Field<Vec<DxgiOutput>>>,
}

#[derive(Clone, Default)]
struct Fake(Rc<RefCell<State>>);

impl WindowsNative for Fake {
    fn display_paths(&mut self) -> Result<Vec<PathReport>, DeviceError> {
        self.0
            .borrow()
            .paths
            .clone()
            .ok_or_else(|| fail("QueryDisplayConfig").into())
    }

    fn dxgi_outputs(&mut self) -> Field<Vec<DxgiOutput>> {
        self.0.borrow().dxgi.clone().unwrap_or(Ok(Vec::new()))
    }
}

impl Fake {
    fn set(&self, paths: Vec<PathReport>, outputs: Vec<DxgiOutput>) {
        let mut s = self.0.borrow_mut();
        s.paths = Some(paths);
        s.dxgi = Some(Ok(outputs));
    }

    fn backend(&self) -> WindowsDisplay<Self> {
        WindowsDisplay::with_native(self.clone())
    }
}

/// HDR active through `INFO_2`, 10 bits, SDR white 240 cd/m².
fn hdr_path(n: u32, name: &str) -> PathReport {
    PathReport {
        adapter_luid: 0x1_0000_2000,
        source_id: n,
        target_id: 100 + n,
        source_name: Ok(format!(r"\\.\DISPLAY{n}")),
        target_name: Ok(TargetName {
            friendly_name: name.into(),
            device_path: format!(r"\\?\DISPLAY#ABC1234#5&1&0&UID{n}"),
            edid_ids: Some((0x1234, 0x5678)),
            connector_instance: n,
            output_technology: 10,
        }),
        color_info: Ok(AdvancedColorInfo {
            flags: info_flags::SUPPORTED | info_flags::ENABLED,
            bits_per_color_channel: 10,
        }),
        color_info_2: Ok(AdvancedColorInfo2 {
            flags: info_2_flags::SUPPORTED
                | info_2_flags::ACTIVE
                | info_2_flags::HDR_SUPPORTED
                | info_2_flags::WCG_SUPPORTED,
            bits_per_color_channel: 10,
            active_color_mode: color_mode::HDR,
        }),
        sdr_white_level: Ok(3000),
        target_active_size: Some((3840, 2160)),
    }
}

fn output(n: u32, color_space: i32) -> DxgiOutput {
    DxgiOutput {
        device_name: format!(r"\\.\DISPLAY{n}"),
        desc1: Ok(OutputDesc1 {
            color_space,
            bits_per_color: 10,
            min_luminance: 0.005,
            max_luminance: 1000.0,
            max_full_frame_luminance: 600.0,
        }),
    }
}

fn pq(n: u32) -> DxgiOutput {
    output(n, dxgi_color_space::RGB_FULL_G2084_NONE_P2020)
}

fn sdr(n: u32) -> DxgiOutput {
    output(n, dxgi_color_space::RGB_FULL_G22_NONE_P709)
}

/// Query of the only device.
fn query_one(path: PathReport, outputs: Vec<DxgiOutput>) -> DeviceQuery {
    let fake = Fake::default();
    fake.set(vec![path], outputs);
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
fn hdr_active() {
    let q = query_one(hdr_path(1, "Monitor"), vec![pq(1)]);
    assert_eq!(q.capabilities.hdr.support(), Support::Yes);
    assert_eq!(q.capabilities.sdr_wcg.support(), Support::Yes);
    let m = &q.mode;
    assert_eq!(value(&m.hdr_on), Some(true));
    assert_eq!(source(&m.hdr_on), "DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2");
    assert_eq!(value(&m.gamut), Some(Gamut::Rec2020));
    assert_eq!(value(&m.wire_depth), Some(10));
    assert_eq!(value(&m.peak_nits), Some(1000.0));
    assert_eq!(value(&m.min_nits), Some(f64::from(0.005f32)));
    assert_eq!(value(&m.full_frame_nits), Some(600.0));
    // 3000 / 1000 × 80 cd/m².
    assert_eq!(value(&m.sdr_white_nits), Some(240.0));
    assert_eq!(value(&m.native_size), Some((3840, 2160)));
}

#[test]
fn hdr_off_keeps_support_separate() {
    let mut p = hdr_path(1, "Monitor");
    p.color_info = Ok(AdvancedColorInfo {
        flags: info_flags::SUPPORTED,
        bits_per_color_channel: 8,
    });
    p.color_info_2 = Ok(AdvancedColorInfo2 {
        flags: info_2_flags::SUPPORTED | info_2_flags::HDR_SUPPORTED,
        bits_per_color_channel: 8,
        active_color_mode: color_mode::SDR,
    });
    let q = query_one(p, vec![sdr(1)]);
    assert_eq!(q.capabilities.hdr.support(), Support::Yes);
    assert!(matches!(q.capabilities.sdr_wcg.support(), Support::No(_)));
    assert_eq!(value(&q.mode.hdr_on), Some(false));
    assert_eq!(value(&q.mode.gamut), Some(Gamut::Srgb));
    assert_eq!(value(&q.mode.wire_depth), Some(8));
    assert_eq!(
        reason(&q.mode.sdr_white_nits),
        "SDR white level applies only with HDR active"
    );
}

/// Advanced Color is enabled for SDR WCG too; it must not set HDR.
#[test]
fn sdr_wcg_is_not_hdr() {
    let mut p = hdr_path(1, "Monitor");
    p.color_info_2 = Ok(AdvancedColorInfo2 {
        flags: info_2_flags::SUPPORTED | info_2_flags::ACTIVE | info_2_flags::WCG_SUPPORTED,
        bits_per_color_channel: 10,
        active_color_mode: color_mode::WCG,
    });
    let q = query_one(p.clone(), vec![sdr(1)]);
    assert!(matches!(q.capabilities.hdr.support(), Support::No(_)));
    assert_eq!(q.capabilities.sdr_wcg.support(), Support::Yes);
    assert_eq!(value(&q.mode.hdr_on), Some(false));
    assert_eq!(value(&q.mode.gamut), None);
    assert_eq!(value(&q.mode.sdr_white_nits), None);

    // Without INFO_2: the legacy flags and DXGI.
    p.color_info_2 = Err(fail("INFO_2"));
    p.color_info = Ok(AdvancedColorInfo {
        flags: info_flags::SUPPORTED | info_flags::ENABLED | info_flags::WIDE_COLOR_ENFORCED,
        bits_per_color_channel: 10,
    });
    let q = query_one(p, vec![sdr(1)]);
    assert_eq!(value(&q.mode.hdr_on), Some(false));
    assert_eq!(q.capabilities.sdr_wcg.support(), Support::Yes);
    assert!(matches!(q.capabilities.hdr.support(), Support::Unknown(_)));
}

#[test]
fn legacy_enabled_alone_does_not_report_hdr() {
    let mut p = hdr_path(1, "Monitor");
    p.color_info_2 = Err(fail("INFO_2"));
    let mut outputs = vec![pq(1)];
    outputs[0].desc1 = Err(fail("IDXGIOutput6::GetDesc1"));
    let q = query_one(p.clone(), outputs);
    assert!(reason(&q.mode.hdr_on).contains("IDXGIOutput6::GetDesc1"));
    assert!(matches!(q.capabilities.hdr.support(), Support::Unknown(_)));
    assert!(matches!(
        q.capabilities.sdr_wcg.support(),
        Support::Unknown(_)
    ));

    // A non-PQ DXGI colour space rules out HDR but not SDR WCG.
    let q = query_one(p.clone(), vec![sdr(1)]);
    assert_eq!(value(&q.mode.hdr_on), Some(false));
    assert_eq!(source(&q.mode.hdr_on), "DXGI_OUTPUT_DESC1");
    assert_eq!(value(&q.mode.gamut), None);

    // The PQ colour space proves HDR.
    let q = query_one(p, vec![pq(1)]);
    assert_eq!(value(&q.mode.hdr_on), Some(true));
    assert_eq!(source(&q.mode.hdr_on), "DXGI_OUTPUT_DESC1");
    assert_eq!(q.capabilities.hdr.support(), Support::Yes);
    assert!(matches!(
        q.capabilities.sdr_wcg.support(),
        Support::Unknown(_)
    ));
    assert_eq!(value(&q.mode.wire_depth), Some(10));
    assert_eq!(
        source(&q.mode.wire_depth),
        "DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO"
    );
}

#[test]
fn legacy_without_advanced_color() {
    let mut p = hdr_path(1, "Monitor");
    p.color_info_2 = Err(fail("INFO_2"));
    p.color_info = Ok(AdvancedColorInfo {
        flags: 0,
        bits_per_color_channel: 8,
    });
    let q = query_one(p, vec![sdr(1)]);
    assert!(matches!(q.capabilities.hdr.support(), Support::No(_)));
    assert!(matches!(q.capabilities.sdr_wcg.support(), Support::No(_)));
    assert_eq!(value(&q.mode.hdr_on), Some(false));
    assert_eq!(value(&q.mode.gamut), Some(Gamut::Srgb));
}

#[test]
fn missing_luminance_and_depth_stay_unknown() {
    let mut p = hdr_path(1, "Monitor");
    p.color_info_2 = Ok(AdvancedColorInfo2 {
        bits_per_color_channel: 0,
        ..p.color_info_2.unwrap()
    });
    p.sdr_white_level = Ok(0);
    p.target_active_size = None;
    let mut o = pq(1);
    o.desc1 = Ok(OutputDesc1 {
        color_space: dxgi_color_space::RGB_FULL_G2084_NONE_P2020,
        bits_per_color: 0,
        min_luminance: 0.0,
        max_luminance: 0.0,
        max_full_frame_luminance: 0.0,
    });
    let m = query_one(p, vec![o]).mode;
    assert_eq!(value(&m.hdr_on), Some(true));
    assert!(reason(&m.wire_depth).contains("0 bits"));
    assert!(reason(&m.peak_nits).contains("MaxLuminance"));
    // A zero minimum is valid, but not without a luminance report.
    assert!(reason(&m.min_nits).contains("MaxLuminance"));
    assert!(reason(&m.full_frame_nits).contains("MaxFullFrameLuminance"));
    assert!(reason(&m.sdr_white_nits).contains("reported 0"));
    assert_eq!(reason(&m.native_size), "path has no target mode");
}

#[test]
fn zero_minimum_with_peak_is_known() {
    let mut o = pq(1);
    if let Ok(d) = &mut o.desc1 {
        d.min_luminance = 0.0;
    }
    let m = query_one(hdr_path(1, "Monitor"), vec![o]).mode;
    assert_eq!(value(&m.min_nits), Some(0.0));
}

#[test]
fn every_failed_field_is_unknown_with_reason() {
    let path = PathReport {
        adapter_luid: 0x2a,
        source_id: 0,
        target_id: 7,
        source_name: Err(fail("GET_SOURCE_NAME")),
        target_name: Err(fail("GET_TARGET_NAME")),
        color_info: Err(fail("GET_ADVANCED_COLOR_INFO")),
        color_info_2: Err(fail("GET_ADVANCED_COLOR_INFO_2")),
        sdr_white_level: Err(fail("GET_SDR_WHITE_LEVEL")),
        target_active_size: None,
    };
    let fake = Fake::default();
    fake.set(vec![path], vec![]);
    fake.0.borrow_mut().dxgi = Some(Err(fail("CreateDXGIFactory1")));
    let mut b = fake.backend();
    let info = b.devices().unwrap().remove(0);
    // The fallback key contains the LUID and target ID.
    assert_eq!(info.id.key, "target:000000000000002a:7");
    assert_eq!(info.name, "Display target 7");
    assert_eq!(info.evidence, Vec::<(String, String)>::new());
    let q = b.query_device(&info.id).unwrap();
    let caps = &q.capabilities;
    assert!(reason(&caps.hdr).contains("GET_ADVANCED_COLOR_INFO_2"));
    assert!(reason(&caps.sdr_wcg).contains("GET_ADVANCED_COLOR_INFO failed"));
    assert!(reason(&caps.wire_depths).contains("active wire depth only"));
    assert!(reason(&caps.icc_profiles).contains("not implemented"));
    let m = &q.mode;
    assert!(reason(&m.hdr_on).contains("CreateDXGIFactory1"));
    assert!(reason(&m.gamut).contains("GET_ADVANCED_COLOR_INFO"));
    assert!(reason(&m.wire_depth).contains("GET_ADVANCED_COLOR_INFO_2"));
    assert!(reason(&m.peak_nits).contains("CreateDXGIFactory1"));
    assert!(reason(&m.min_nits).contains("CreateDXGIFactory1"));
    assert!(reason(&m.full_frame_nits).contains("CreateDXGIFactory1"));
    assert!(reason(&m.sdr_white_nits).contains("GET_ADVANCED_COLOR_INFO"));
    assert!(reason(&m.native_size).contains("no target mode"));
}

#[test]
fn dxgi_failure_and_unmatched_output() {
    let fake = Fake::default();
    fake.set(vec![hdr_path(1, "Monitor")], vec![pq(2)]);
    let mut b = fake.backend();
    let id = b.devices().unwrap().remove(0).id;
    let m = b.query_device(&id).unwrap().mode;
    assert_eq!(reason(&m.peak_nits), r"no DXGI output for \\.\DISPLAY1");
    // INFO_2 still gives the active state.
    assert_eq!(value(&m.hdr_on), Some(true));

    fake.0.borrow_mut().dxgi = Some(Err(fail("CreateDXGIFactory1")));
    let m = b.query_device(&id).unwrap().mode;
    assert!(reason(&m.peak_nits).starts_with("CreateDXGIFactory1 failed"));
}

#[test]
fn duplicate_names_have_distinct_devices() {
    let fake = Fake::default();
    let mut second = hdr_path(2, "Monitor");
    second.color_info_2 = Ok(AdvancedColorInfo2 {
        active_color_mode: color_mode::SDR,
        ..second.color_info_2.unwrap()
    });
    let mut second_out = sdr(2);
    if let Ok(d) = &mut second_out.desc1 {
        d.max_luminance = 400.0;
    }
    fake.set(
        vec![hdr_path(1, "Monitor"), second],
        vec![pq(1), second_out],
    );
    let mut b = fake.backend();
    let devices = b.devices().unwrap();
    assert_eq!(devices.len(), 2);
    assert_eq!(devices[0].name, devices[1].name);
    assert_ne!(devices[0].id, devices[1].id);
    let first = b.query_device(&devices[0].id).unwrap();
    let second = b.query_device(&devices[1].id).unwrap();
    assert_eq!(value(&first.mode.hdr_on), Some(true));
    assert_eq!(value(&second.mode.hdr_on), Some(false));
    assert_eq!(value(&first.mode.peak_nits), Some(1000.0));
    assert_eq!(value(&second.mode.peak_nits), Some(400.0));
}

#[test]
fn shared_device_path_is_ambiguous() {
    let fake = Fake::default();
    let mut twin = hdr_path(2, "Monitor");
    twin.target_name = hdr_path(1, "Monitor").target_name;
    fake.set(vec![hdr_path(1, "Monitor"), twin], vec![pq(1), pq(2)]);
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
fn topology_changes() {
    let fake = Fake::default();
    fake.set(vec![hdr_path(1, "A"), hdr_path(2, "B")], vec![pq(1), pq(2)]);
    let mut b = fake.backend();
    let devices = b.devices().unwrap();
    let rev =
        |b: &mut WindowsDisplay<Fake>, id: &DeviceId| b.query_device(id).unwrap().topology_revision;
    let base = rev(&mut b, &devices[0].id);

    // New enumeration order and a new adapter LUID after a restart.
    let mut restarted = vec![hdr_path(2, "B"), hdr_path(1, "A")];
    for p in &mut restarted {
        p.adapter_luid = 0x9_0000_0001;
    }
    fake.set(restarted, vec![pq(1), pq(2)]);
    assert_eq!(rev(&mut b, &devices[0].id), base);

    // Monitor B is replaced on the same connector.
    let mut replaced = hdr_path(2, "B");
    if let Ok(t) = &mut replaced.target_name {
        t.device_path = r"\\?\DISPLAY#XYZ9999#5&1&0&UID2".into();
        t.edid_ids = Some((0x4321, 0x0001));
    }
    fake.set(vec![hdr_path(1, "A"), replaced], vec![pq(1), pq(2)]);
    assert_ne!(rev(&mut b, &devices[0].id), base);
    assert_eq!(
        b.query_device(&devices[1].id).unwrap_err(),
        DeviceError::Disconnected(devices[1].id.clone())
    );

    // Monitor B is disconnected.
    fake.set(vec![hdr_path(1, "A")], vec![pq(1)]);
    assert_eq!(b.devices().unwrap().len(), 1);
    assert!(matches!(
        b.query_device(&devices[1].id),
        Err(DeviceError::Disconnected(_))
    ));
}

#[test]
fn query_display_config_failure_is_an_error() {
    let mut b = Fake::default().backend();
    assert!(matches!(
        b.devices(),
        Err(DeviceError::Native { api, code: Some(NOT_SUPPORTED), .. }) if api == "QueryDisplayConfig"
    ));
}

#[test]
fn profile_control_is_explicitly_unsupported() {
    let fake = Fake::default();
    fake.set(vec![hdr_path(1, "Monitor")], vec![pq(1)]);
    let mut b = fake.backend();
    let id = b.devices().unwrap().remove(0).id;
    assert_eq!(b.profile_targets(&id).unwrap(), vec![]);
    let target = ProfileTarget {
        device: id,
        role: ProfileRole::Standard,
        scope: ProfileScope {
            user: UserScope::Current,
            host: None,
        },
    };
    assert!(matches!(
        b.profile_state(&target),
        Err(DeviceError::Unsupported { reason, .. }) if reason.contains("not implemented")
    ));
}
