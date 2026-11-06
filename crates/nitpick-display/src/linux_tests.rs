use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use nitpick_core::device::{
    DeviceError, DeviceId, DeviceQuery, DisplayDeviceControl, ProfileRole, ProfileScope,
    ProfileTarget, Report, UserScope,
};
use nitpick_core::driver::{Gamut, Support};

use super::{
    COLORD_EDID_MD5, ColordDevice, ColordReport, ConnectorReport, KmsConnector, LinuxDisplay,
    LinuxNative, Property,
};
use crate::common::{Field, NativeFailure};
use crate::edid::md5_hex;
use crate::edid::tests::{Spec, build, cta, hdr};

const EACCES: i64 = 13;
const UID: u32 = 1000;

fn fail(api: &'static str) -> NativeFailure {
    NativeFailure { api, code: EACCES }
}

#[derive(Default)]
struct State {
    /// `None`: enumeration fails.
    connectors: Option<Vec<ConnectorReport>>,
    kms: BTreeMap<String, Field<Vec<KmsConnector>>>,
    colord: Option<Result<ColordReport, String>>,
    kms_calls: Vec<String>,
    colord_calls: usize,
}

#[derive(Clone, Default)]
struct Fake(Rc<RefCell<State>>);

impl LinuxNative for Fake {
    fn connectors(&mut self) -> Result<Vec<ConnectorReport>, DeviceError> {
        self.0
            .borrow()
            .connectors
            .clone()
            .ok_or_else(|| fail("read_dir /sys/class/drm").into())
    }

    fn kms(&mut self, card: &str) -> Field<Vec<KmsConnector>> {
        let mut s = self.0.borrow_mut();
        s.kms_calls.push(card.to_owned());
        s.kms.get(card).cloned().unwrap_or(Ok(Vec::new()))
    }

    fn colord(&mut self) -> Result<ColordReport, String> {
        let mut s = self.0.borrow_mut();
        s.colord_calls += 1;
        s.colord
            .clone()
            .unwrap_or_else(|| Err("colord is not running".into()))
    }
}

impl Fake {
    /// Connectors, KMS state of `card1` and colord devices for every EDID.
    fn set(&self, connectors: Vec<ConnectorReport>, kms: Vec<KmsConnector>) {
        let devices = connectors
            .iter()
            .filter_map(|c| c.edid.as_ref().ok().filter(|e| !e.is_empty()))
            .map(|e| colord_device(e, UID))
            .collect();
        let mut s = self.0.borrow_mut();
        s.connectors = Some(connectors);
        s.kms.insert("card1".into(), Ok(kms));
        s.colord = Some(Ok(ColordReport { uid: UID, devices }));
    }

    fn backend(&self) -> LinuxDisplay<Self> {
        LinuxDisplay::with_native(self.clone())
    }
}

fn colord_device(edid: &[u8], owner: u32) -> ColordDevice {
    ColordDevice {
        id: format!("xrandr-{}", md5_hex(edid)),
        owner,
        metadata: BTreeMap::from([(COLORD_EDID_MD5.to_owned(), md5_hex(edid))]),
    }
}

/// An HDR10 display: PQ and HLG, 400 cd/m² peak, 200 cd/m² frame
/// average, 4 cd/m² minimum.
fn hdr_edid(serial_text: &str) -> Vec<u8> {
    build(&Spec {
        name: Some("HDR Monitor"),
        serial_text: Some(serial_text),
        extensions: vec![cta(&[&hdr(&[0x0d, 0x01, 96, 64, 255])])],
        ..Spec::default()
    })
}

fn sdr_edid() -> Vec<u8> {
    build(&Spec {
        extensions: vec![cta(&[&[(2 << 5) | 1, 16]])],
        ..Spec::default()
    })
}

fn connector(n: u32, edid: Vec<u8>) -> ConnectorReport {
    ConnectorReport {
        card: "card1".into(),
        connector: format!("DP-{n}"),
        bus: Ok("0000:01:00.0".into()),
        connector_id: Ok(100 + n),
        status: "connected".into(),
        edid: Ok(edid),
    }
}

/// `struct hdr_output_metadata` with this EOTF, padded to 32 bytes.
fn metadata_blob(kind: u32, eotf: u8) -> Vec<u8> {
    let mut b = kind.to_ne_bytes().to_vec();
    b.push(eotf);
    b.resize(32, 0);
    b
}

/// HDR10 active: PQ metadata, BT.2020 colorimetry, 3840 × 2160.
fn kms(n: u32) -> KmsConnector {
    KmsConnector {
        connector_id: 100 + n,
        hdr_output_metadata: Property::Value(Some(metadata_blob(0, 2))),
        colorspace: Property::Value("BT2020_RGB".into()),
        crtc_mode: Ok(Some((3840, 2160))),
    }
}

fn query(fake: &Fake) -> DeviceQuery {
    let mut b = fake.backend();
    let id = b.devices().unwrap().remove(0).id;
    b.query_device(&id).unwrap()
}

fn query_one(c: ConnectorReport, k: KmsConnector) -> DeviceQuery {
    let fake = Fake::default();
    fake.set(vec![c], vec![k]);
    query(&fake)
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
    let edid = hdr_edid("SN0001");
    let q = query_one(connector(1, edid.clone()), kms(1));
    assert_eq!(q.device.id.backend, "linux");
    assert_eq!(q.device.id.key, "0000:01:00.0/DP-1/DEL40E7-SN0001");
    assert_eq!(q.device.name, "HDR Monitor");
    assert_eq!(
        q.device.evidence,
        vec![
            ("connector".into(), "DP-1".into()),
            ("edid_ids".into(), "10AC:40E7".into()),
            ("serial".into(), "01010101".into()),
            ("serial_text".into(), "SN0001".into()),
            ("edid_md5".into(), md5_hex(&edid)),
        ]
    );
    let c = &q.capabilities;
    assert_eq!(c.hdr.support(), Support::Yes);
    assert_eq!(
        source(&c.hdr),
        "EDID HDR static metadata; KMS connector properties"
    );
    let m = &q.mode;
    assert_eq!(value(&m.hdr_on), Some(true));
    assert_eq!(source(&m.hdr_on), "KMS HDR_OUTPUT_METADATA");
    assert_eq!(value(&m.gamut), Some(Gamut::Rec2020));
    assert_eq!(value(&m.peak_nits), Some(400.0));
    assert_eq!(value(&m.full_frame_nits), Some(200.0));
    assert_eq!(value(&m.min_nits), Some(4.0));
    assert_eq!(source(&m.peak_nits), "EDID HDR static metadata");
    assert_eq!(value(&m.native_size), Some((3840, 2160)));
    assert_eq!(source(&m.native_size), "KMS CRTC mode");
    let icc = reason(&c.icc_profiles);
    assert!(icc.contains("not implemented"));
    assert!(icc.contains(&format!("colord device xrandr-{}", md5_hex(&edid))));
}

/// Fields without a Linux report stay unknown; no display value is
/// guessed for them.
#[test]
fn unreported_fields_stay_unknown() {
    let q = query_one(connector(1, hdr_edid("SN0001")), kms(1));
    assert!(reason(&q.capabilities.sdr_wcg).contains("SDR wide-colour"));
    assert!(reason(&q.capabilities.wire_depths).contains("max bpc is only a limit"));
    assert!(reason(&q.mode.wire_depth).contains("link depth"));
    assert!(reason(&q.mode.sdr_white_nits).contains("SDR white"));
    assert!(reason(&q.mode.edr).contains("macOS"));
}

#[test]
fn only_connected_connectors_are_devices() {
    let fake = Fake::default();
    let mut off = connector(2, Vec::new());
    off.status = "disconnected".into();
    let mut writeback = connector(3, Vec::new());
    writeback.status = "unknown".into();
    fake.set(vec![connector(1, sdr_edid()), off, writeback], vec![kms(1)]);
    let devices = fake.backend().devices().unwrap();
    assert_eq!(devices.len(), 1);
    assert!(devices[0].id.key.contains("/DP-1/"));
}

/// Listing reads sysfs only. A query opens only the device's card and
/// reads colord once.
#[test]
fn listing_opens_no_device_node() {
    let fake = Fake::default();
    fake.set(vec![connector(1, sdr_edid())], vec![kms(1)]);
    let mut b = fake.backend();
    let id = b.devices().unwrap().remove(0).id;
    b.profile_targets(&id).unwrap();
    assert_eq!(fake.0.borrow().kms_calls, Vec::<String>::new());
    assert_eq!(fake.0.borrow().colord_calls, 0);
    b.query_device(&id).unwrap();
    assert_eq!(fake.0.borrow().kms_calls, ["card1"]);
    assert_eq!(fake.0.borrow().colord_calls, 1);
}

#[test]
fn sdr_display_with_hdr_off() {
    let mut k = kms(1);
    k.hdr_output_metadata = Property::Value(None);
    k.colorspace = Property::Value("Default".into());
    let q = query_one(connector(1, sdr_edid()), k);
    assert!(matches!(q.capabilities.hdr.support(), Support::No(_)));
    assert_eq!(source(&q.capabilities.hdr), "EDID");
    assert_eq!(value(&q.mode.hdr_on), Some(false));
    assert_eq!(source(&q.mode.hdr_on), "KMS HDR_OUTPUT_METADATA");
    // A compositor can send the panel gamut without colorimetry.
    assert!(reason(&q.mode.gamut).contains("Default"));
    for r in [&q.mode.peak_nits, &q.mode.min_nits, &q.mode.full_frame_nits] {
        assert_eq!(reason(r), "EDID has no HDR static metadata");
    }
}

/// A driver without the HDR infoframe property cannot signal HDR, even to
/// an HDR display.
#[test]
fn driver_without_hdr_properties() {
    let mut k = kms(1);
    k.hdr_output_metadata = Property::Absent;
    k.colorspace = Property::Absent;
    let q = query_one(connector(1, hdr_edid("SN0001")), k);
    assert!(matches!(q.capabilities.hdr.support(), Support::No(_)));
    assert_eq!(source(&q.capabilities.hdr), "KMS connector properties");
    assert_eq!(value(&q.mode.hdr_on), Some(false));
    assert!(reason(&q.mode.gamut).contains("no Colorspace property"));
    assert_eq!(value(&q.mode.peak_nits), Some(400.0));
}

/// Without seat access the card cannot be opened. The EDID fields remain.
#[test]
fn unreadable_kms() {
    let fake = Fake::default();
    fake.set(vec![connector(1, hdr_edid("SN0001"))], Vec::new());
    fake.0
        .borrow_mut()
        .kms
        .insert("card1".into(), Err(fail("open /dev/dri card")));
    let q = query(&fake);
    let expected = "card1: open /dev/dri card failed (0xd)";
    assert_eq!(reason(&q.capabilities.hdr), expected);
    assert_eq!(reason(&q.mode.hdr_on), expected);
    assert_eq!(reason(&q.mode.gamut), expected);
    assert_eq!(reason(&q.mode.native_size), expected);
    assert_eq!(value(&q.mode.peak_nits), Some(400.0));
    assert_eq!(q.device.name, "HDR Monitor");

    // The EDID alone can rule HDR out.
    fake.0.borrow_mut().connectors = Some(vec![connector(1, sdr_edid())]);
    let q = query(&fake);
    assert!(matches!(q.capabilities.hdr.support(), Support::No(_)));
}

#[test]
fn connector_missing_from_kms() {
    let q = query_one(connector(1, hdr_edid("SN0001")), kms(7));
    assert_eq!(reason(&q.mode.hdr_on), "card1 has no KMS connector 101");

    let mut c = connector(1, hdr_edid("SN0001"));
    c.connector_id = Err(fail("read connector_id"));
    let q = query_one(c, kms(1));
    assert!(reason(&q.mode.native_size).starts_with("read connector_id failed"));
}

#[test]
fn hdr_output_metadata_blobs() {
    let hdr_on = |p: Property<Option<Vec<u8>>>| {
        let mut k = kms(1);
        k.hdr_output_metadata = p;
        query_one(connector(1, hdr_edid("SN0001")), k).mode.hdr_on
    };
    let blob = |kind, eotf| Property::Value(Some(metadata_blob(kind, eotf)));
    assert_eq!(value(&hdr_on(blob(0, 3))), Some(true), "HLG");
    assert_eq!(value(&hdr_on(blob(0, 0))), Some(false), "SDR gamma");
    assert!(reason(&hdr_on(blob(0, 1))).contains("EOTF 1"));
    assert!(reason(&hdr_on(blob(1, 2))).contains("metadata type 1"));
    assert!(reason(&hdr_on(Property::Value(Some(vec![0; 4])))).contains("4 bytes"));
    let failed = hdr_on(Property::Failed(fail("DRM_IOCTL_MODE_GETPROPBLOB")));
    assert!(reason(&failed).starts_with("DRM_IOCTL_MODE_GETPROPBLOB failed"));
}

#[test]
fn colorspace_names() {
    let gamut = |name: &str| {
        let mut k = kms(1);
        k.colorspace = Property::Value(name.into());
        query_one(connector(1, hdr_edid("SN0001")), k).mode.gamut
    };
    assert_eq!(value(&gamut("BT2020_YCC")), Some(Gamut::Rec2020));
    assert_eq!(value(&gamut("BT2020_CYCC")), Some(Gamut::Rec2020));
    assert_eq!(value(&gamut("DCI-P3_RGB_D65")), Some(Gamut::P3));
    assert!(reason(&gamut("opRGB")).contains("opRGB is not a known gamut"));
    assert!(reason(&gamut("#42")).contains("#42"));
}

#[test]
fn luminance_codes_absent_or_zero() {
    let edid = |payload: &[u8]| {
        build(&Spec {
            extensions: vec![cta(&[&hdr(payload)])],
            ..Spec::default()
        })
    };
    let q = query_one(connector(1, edid(&[0x04, 0x01, 0, 0, 10])), kms(1));
    assert!(reason(&q.mode.peak_nits).contains("max luminance code 0"));
    assert!(reason(&q.mode.full_frame_nits).contains("frame-average luminance code 0"));
    // The minimum scales an unknown peak.
    assert!(reason(&q.mode.min_nits).contains("max luminance code 0"));

    let q = query_one(connector(1, edid(&[0x04, 0x01, 96])), kms(1));
    assert_eq!(value(&q.mode.peak_nits), Some(400.0));
    assert!(reason(&q.mode.full_frame_nits).contains("no max frame-average"));
    assert!(reason(&q.mode.min_nits).contains("no min luminance"));

    // Code 0 of a minimum with a known peak is 0 cd/m².
    let q = query_one(connector(1, edid(&[0x04, 0x01, 96, 64, 0])), kms(1));
    assert_eq!(value(&q.mode.min_nits), Some(0.0));
}

/// A `DisplayID` extension can report HDR in a form that is not read.
#[test]
fn displayid_hdr_is_unknown() {
    let mut displayid = [0u8; 128];
    displayid[0] = 0x70;
    let edid = build(&Spec {
        extensions: vec![displayid],
        ..Spec::default()
    });
    let q = query_one(connector(1, edid.clone()), kms(1));
    assert!(reason(&q.capabilities.hdr).contains("DisplayID"));

    let mut k = kms(1);
    k.hdr_output_metadata = Property::Absent;
    let q = query_one(connector(1, edid), k);
    assert!(matches!(q.capabilities.hdr.support(), Support::No(_)));
}

#[test]
fn connector_without_edid() {
    let q = query_one(connector(1, Vec::new()), kms(1));
    assert_eq!(q.device.id.key, "0000:01:00.0/DP-1/no-edid");
    assert_eq!(q.device.name, "DP-1");
    assert_eq!(q.device.evidence, vec![("connector".into(), "DP-1".into())]);
    assert_eq!(reason(&q.capabilities.hdr), "connector has no EDID");
    assert_eq!(reason(&q.mode.peak_nits), "connector has no EDID");
    assert_eq!(
        reason(&q.capabilities.icc_profiles),
        "no EDID to match a colord device"
    );
    // KMS fields do not need the EDID.
    assert_eq!(value(&q.mode.hdr_on), Some(true));

    let mut c = connector(1, Vec::new());
    c.edid = Err(fail("read edid"));
    let q = query_one(c, kms(1));
    assert!(reason(&q.capabilities.hdr).starts_with("read edid failed"));

    let mut bad = sdr_edid();
    bad[0] = 0xff;
    let q = query_one(connector(1, bad), kms(1));
    assert_eq!(reason(&q.mode.peak_nits), "no EDID header");
}

/// The card number can change after a restart; the bus ID does not.
#[test]
fn key_without_bus_id_uses_the_card() {
    let mut c = connector(1, sdr_edid());
    c.bus = Err(fail("readlink drm card device"));
    let q = query_one(c, kms(1));
    assert_eq!(q.device.id.key, "card1/DP-1/DEL40E7-SN0001");
}

#[test]
fn numeric_serial_without_serial_text() {
    let edid = build(&Spec {
        serial_text: None,
        name: None,
        ..Spec::default()
    });
    let q = query_one(connector(1, edid), kms(1));
    assert_eq!(q.device.id.key, "0000:01:00.0/DP-1/DEL40E7-01010101");
    assert_eq!(q.device.name, "DP-1");
}

/// Another unit on the same port, or the same unit on another port, is a
/// new device. The old ID fails as disconnected.
#[test]
fn output_replacement() {
    let fake = Fake::default();
    fake.set(vec![connector(1, hdr_edid("SN0001"))], vec![kms(1)]);
    let mut b = fake.backend();
    let old = b.devices().unwrap().remove(0).id;
    let old_revision = b.query_device(&old).unwrap().topology_revision;

    fake.set(vec![connector(1, hdr_edid("SN0002"))], vec![kms(1)]);
    let new = b.devices().unwrap().remove(0).id;
    assert_ne!(new, old);
    assert!(matches!(
        b.query_device(&old),
        Err(DeviceError::Disconnected(id)) if id == old
    ));
    assert!(matches!(
        b.profile_targets(&old),
        Err(DeviceError::Disconnected(_))
    ));
    assert_ne!(
        b.query_device(&new).unwrap().topology_revision,
        old_revision
    );

    fake.set(vec![connector(2, hdr_edid("SN0001"))], vec![kms(2)]);
    assert!(matches!(
        b.query_device(&old),
        Err(DeviceError::Disconnected(_))
    ));
}

#[test]
fn duplicate_native_reports_are_ambiguous() {
    let fake = Fake::default();
    let c = connector(1, sdr_edid());
    fake.set(vec![c.clone(), c], vec![kms(1)]);
    let mut b = fake.backend();
    let id = b.devices().unwrap().remove(0).id;
    assert!(matches!(
        b.query_device(&id),
        Err(DeviceError::AmbiguousIdentity(_))
    ));
}

/// Identical EDIDs, for example two monitors without serial numbers, map
/// to one colord device. Neither output can be matched.
#[test]
fn identical_edids_have_no_colord_match() {
    let fake = Fake::default();
    let edid = sdr_edid();
    fake.set(
        vec![connector(1, edid.clone()), connector(2, edid)],
        vec![kms(1), kms(2)],
    );
    let mut b = fake.backend();
    let ids: Vec<DeviceId> = b.devices().unwrap().into_iter().map(|d| d.id).collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    for id in &ids {
        let q = b.query_device(id).unwrap();
        assert_eq!(
            reason(&q.capabilities.icc_profiles),
            "2 outputs have the same EDID"
        );
    }
}

#[test]
fn colord_device_matching() {
    let edid = sdr_edid();
    let icc = |devices: Vec<ColordDevice>| {
        let fake = Fake::default();
        fake.set(vec![connector(1, edid.clone())], vec![kms(1)]);
        fake.0.borrow_mut().colord = Some(Ok(ColordReport { uid: UID, devices }));
        query(&fake).capabilities.icc_profiles
    };
    let none = icc(Vec::new());
    assert!(matches!(none.support(), Support::No(_)));
    assert_eq!(source(&none), "colord GetDevicesByKind");

    let other = colord_device(&sdr_edid()[..128], UID);
    assert!(matches!(icc(vec![other]).support(), Support::No(_)));

    let twice = icc(vec![colord_device(&edid, UID), colord_device(&edid, UID)]);
    assert_eq!(reason(&twice), "2 colord devices match the EDID");

    let foreign = icc(vec![colord_device(&edid, 1001)]);
    assert!(reason(&foreign).contains("has owner 1001"));

    let mixed = icc(vec![colord_device(&edid, 1001), colord_device(&edid, UID)]);
    assert!(reason(&mixed).contains("not implemented"));
}

/// Without the profile service, the other fields remain.
#[test]
fn colord_absent() {
    let fake = Fake::default();
    fake.set(vec![connector(1, hdr_edid("SN0001"))], vec![kms(1)]);
    fake.0.borrow_mut().colord = Some(Err("colord is not running".into()));
    let q = query(&fake);
    assert_eq!(
        reason(&q.capabilities.icc_profiles),
        "colord: colord is not running"
    );
    assert_eq!(q.capabilities.hdr.support(), Support::Yes);
    assert_eq!(value(&q.mode.hdr_on), Some(true));
}

#[test]
fn enumeration_failure_is_an_error() {
    let mut b = Fake::default().backend();
    let err = b.devices().unwrap_err();
    assert!(matches!(err, DeviceError::Native { api, .. } if api == "read_dir /sys/class/drm"));
}

#[test]
fn profile_operations_are_unsupported() {
    let fake = Fake::default();
    fake.set(vec![connector(1, sdr_edid())], vec![kms(1)]);
    let mut b = fake.backend();
    let id = b.devices().unwrap().remove(0).id;
    assert_eq!(b.profile_targets(&id).unwrap(), Vec::new());
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
