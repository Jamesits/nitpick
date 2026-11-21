use nitpick_core::device::{DeviceError, DeviceId, GammaRamp, Report};

use super::{RAMP_LEN, WindowsSetupNative, ramp};
use crate::common::{Field, NativeFailure};
use crate::windows::{DxgiOutput, PathReport, TargetName, WindowsDisplay, WindowsNative};

struct Fake {
    source_name: Field<String>,
    ramp: Result<Vec<u16>, DeviceError>,
    calibration: Result<bool, DeviceError>,
    /// GDI names that the ramp read received.
    ramp_reads: Vec<String>,
}

impl WindowsNative for Fake {
    fn display_paths(&mut self) -> Result<Vec<PathReport>, DeviceError> {
        Ok(vec![PathReport {
            adapter_luid: 1,
            target_adapter_luid: 1,
            source_id: 0,
            target_id: 100,
            source_name: self.source_name.clone(),
            target_name: Ok(TargetName {
                friendly_name: "Monitor".into(),
                device_path: KEY.into(),
                edid_ids: None,
                connector_instance: 0,
                output_technology: 10,
            }),
            color_info: Err(not_supported()),
            color_info_2: Err(not_supported()),
            sdr_white_level: Err(not_supported()),
            target_active_size: None,
        }])
    }

    fn dxgi_outputs(&mut self) -> Field<Vec<DxgiOutput>> {
        Ok(Vec::new())
    }
}

impl WindowsSetupNative for Fake {
    fn gamma_ramp(&mut self, gdi_name: &str) -> Result<Vec<u16>, DeviceError> {
        self.ramp_reads.push(gdi_name.into());
        self.ramp.clone()
    }

    fn calibration_management(&mut self) -> Result<bool, DeviceError> {
        self.calibration.clone()
    }
}

const KEY: &str = r"\?\DISPLAY#ABC1234#5&1&0&UID1";

fn not_supported() -> NativeFailure {
    NativeFailure {
        api: "DisplayConfigGetDeviceInfo",
        code: 0x32,
    }
}

fn id() -> DeviceId {
    DeviceId {
        backend: "windows".into(),
        key: KEY.into(),
    }
}

fn identity(form: impl Fn(u16) -> u16) -> Vec<u16> {
    (0..3).flat_map(|_| (0..256).map(&form)).collect()
}

fn fake(ramp: Result<Vec<u16>, DeviceError>) -> Fake {
    Fake {
        source_name: Ok(r"\.\DISPLAY1".into()),
        ramp,
        calibration: Ok(false),
        ramp_reads: Vec::new(),
    }
}

#[test]
fn both_identity_forms_are_identity() {
    assert_eq!(ramp(&identity(|i| i * 257)), Ok(GammaRamp::Identity));
    assert_eq!(ramp(&identity(|i| i << 8)), Ok(GammaRamp::Identity));
}

#[test]
fn other_ramps_are_custom_with_distinct_digests() {
    let mut one_step = identity(|i| i * 257);
    one_step[256 + 128] += 1;
    // Each channel in another identity form is still identity per channel,
    // but a mix within one channel is not.
    let mut mixed = identity(|i| i << 8);
    mixed[255] = 0xffff;
    let a = ramp(&one_step).unwrap();
    let b = ramp(&mixed).unwrap();
    assert!(matches!(a, GammaRamp::Custom { .. }));
    assert!(matches!(b, GammaRamp::Custom { .. }));
    assert_ne!(a, b);
    assert_eq!(ramp(&one_step), Ok(a));
}

#[test]
fn ramp_of_wrong_length_is_rejected() {
    let reason = ramp(&[0; RAMP_LEN - 1]).unwrap_err();
    assert_eq!(reason, "GetDeviceGammaRamp gave 767 entries, expected 768");
}

#[test]
fn setup_reads_the_ramp_of_the_gdi_source() {
    let mut b = WindowsDisplay::with_native(fake(Ok(identity(|i| i * 257))));
    let s = b.setup_of(&id()).unwrap();
    assert_eq!(b.native.ramp_reads, vec![r"\.\DISPLAY1".to_owned()]);
    assert_eq!(s.device, id());
    assert_eq!(
        s.gamma_ramp,
        Report::known(GammaRamp::Identity, "GetDeviceGammaRamp")
    );
    assert_eq!(
        s.os_calibration_loader,
        Report::known(false, "WcsGetCalibrationManagementState")
    );
    // Not read: never taken as off.
    assert!(matches!(s.external_calibration_loader, Report::Unknown(_)));
    assert!(matches!(s.gpu_gamut_clamp, Report::Unknown(_)));
    assert!(matches!(s.dithering, Report::Unknown(_)));
}

#[test]
fn failed_reads_are_unknown_with_reason() {
    let mut f = fake(Err(DeviceError::Native {
        api: "GetDeviceGammaRamp".into(),
        code: None,
        message: "no gamma ramp".into(),
    }));
    f.calibration = Err(DeviceError::Unsupported {
        operation: "calibration state".into(),
        reason: "mscms.dll has no WcsGetCalibrationManagementState".into(),
    });
    let mut b = WindowsDisplay::with_native(f);
    let s = b.setup_of(&id()).unwrap();
    assert_eq!(
        s.gamma_ramp,
        Report::unknown("GetDeviceGammaRamp failed: no gamma ramp")
    );
    assert_eq!(
        s.os_calibration_loader,
        Report::unknown(
            "calibration state not supported: mscms.dll has no WcsGetCalibrationManagementState"
        )
    );

    let mut f = fake(Ok(identity(|i| i * 257)));
    f.source_name = Err(not_supported());
    let mut b = WindowsDisplay::with_native(f);
    let s = b.setup_of(&id()).unwrap();
    assert_eq!(b.native.ramp_reads, Vec::<String>::new());
    assert_eq!(
        s.gamma_ramp,
        Report::unknown("source name: DisplayConfigGetDeviceInfo failed (0x32)")
    );
}

#[test]
fn unknown_device_is_disconnected() {
    let mut b = WindowsDisplay::with_native(fake(Ok(identity(|i| i * 257))));
    let other = DeviceId {
        backend: "windows".into(),
        key: "other".into(),
    };
    assert_eq!(b.setup_of(&other), Err(DeviceError::Disconnected(other)));
}
