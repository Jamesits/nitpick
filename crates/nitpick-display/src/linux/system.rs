//! Sysfs, KMS and colord calls. None needs a particular thread.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::Path;
use std::time::Duration;

use drm::control::{Device as _, connector, property};
use nitpick_core::device::DeviceError;
use zbus::OwnedObjectPath;
use zbus::proxy::CacheProperties;

use super::{ColordDevice, ColordReport, ConnectorReport, KmsConnector, LinuxNative, Property};
use crate::common::{Field, NativeFailure, unsupported};

const SYSFS_DRM: &str = "/sys/class/drm";
const COLORD: &str = "org.freedesktop.ColorManager";

/// Bound on each colord call, for a service that does not answer. Not
/// measured; equal to the macOS main-thread wait.
const COLORD_TIMEOUT: Duration = Duration::from_secs(2);

pub struct System;

fn failure(api: &'static str, e: &io::Error) -> NativeFailure {
    NativeFailure {
        api,
        code: e.raw_os_error().map_or(-1, i64::from),
    }
}

fn read_trimmed(path: &Path) -> io::Result<String> {
    Ok(fs::read_to_string(path)?.trim().to_owned())
}

/// Reading these attributes does not probe the connector.
fn connector(entry: &Path, card: &str, name: &str) -> ConnectorReport {
    let card_device = Path::new(SYSFS_DRM).join(card).join("device");
    ConnectorReport {
        card: card.to_owned(),
        connector: name.to_owned(),
        bus: fs::read_link(&card_device)
            .map_err(|e| failure("readlink drm card device", &e))
            .and_then(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .ok_or_else(|| {
                        failure(
                            "readlink drm card device",
                            &io::ErrorKind::InvalidData.into(),
                        )
                    })
            }),
        connector_id: read_trimmed(&entry.join("connector_id"))
            .map_err(|e| failure("read connector_id", &e))
            .and_then(|s| {
                s.parse()
                    .map_err(|_| failure("parse connector_id", &io::ErrorKind::InvalidData.into()))
            }),
        status: read_trimmed(&entry.join("status")).unwrap_or_else(|e| format!("unreadable: {e}")),
        edid: fs::read(entry.join("edid")).map_err(|e| failure("read edid", &e)),
    }
}

struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl drm::Device for Card {}
impl drm::control::Device for Card {}

/// The first process to open a card without a DRM master becomes its
/// master. Dropping it at once lets a compositor that starts during the
/// query take the card. The call fails, harmlessly, when this process is
/// not master. Std opens the file with `O_CLOEXEC`.
fn open_card(card: &str) -> Field<Card> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(Path::new("/dev/dri").join(card))
        .map_err(|e| failure("open /dev/dri card", &e))?;
    let card = Card(file);
    let _ = drm::Device::release_master_lock(&card);
    Ok(card)
}

/// The mode of the CRTC that drives the connector. `force_probe` is false:
/// a probe is slow and can send hotplug events to the compositor.
fn crtc_mode(card: &Card, handle: connector::Handle) -> Field<Option<(u32, u32)>> {
    let info = card
        .get_connector(handle, false)
        .map_err(|e| failure("DRM_IOCTL_MODE_GETCONNECTOR", &e))?;
    info.current_encoder().map_or(Ok(None), |enc| {
        let encoder = card
            .get_encoder(enc)
            .map_err(|e| failure("DRM_IOCTL_MODE_GETENCODER", &e))?;
        let Some(crtc) = encoder.crtc() else {
            return Ok(None);
        };
        let crtc = card
            .get_crtc(crtc)
            .map_err(|e| failure("DRM_IOCTL_MODE_GETCRTC", &e))?;
        Ok(crtc
            .mode()
            .map(|m| (u32::from(m.size().0), u32::from(m.size().1))))
    })
}

/// A property is absent only if the name of every property was read.
fn kms_connector(card: &Card, handle: connector::Handle) -> KmsConnector {
    let mut hdr_output_metadata = Property::Absent;
    let mut colorspace = Property::Absent;
    let mut unnamed = None;
    match card.get_properties(handle) {
        Ok(set) => {
            let (ids, values) = set.as_props_and_values();
            for (&id, &raw) in ids.iter().zip(values) {
                let prop = match card.get_property(id) {
                    Ok(prop) => prop,
                    Err(e) => {
                        unnamed = Some(failure("DRM_IOCTL_MODE_GETPROPERTY", &e));
                        continue;
                    }
                };
                match prop.name().to_bytes() {
                    b"HDR_OUTPUT_METADATA" => {
                        hdr_output_metadata = match raw {
                            0 => Property::Value(None),
                            blob => card.get_property_blob(blob).map_or_else(
                                |e| Property::Failed(failure("DRM_IOCTL_MODE_GETPROPBLOB", &e)),
                                |b| Property::Value(Some(b)),
                            ),
                        };
                    }
                    b"Colorspace" => {
                        colorspace = match prop.value_type().convert_value(raw) {
                            property::Value::Enum(Some(v)) => {
                                Property::Value(v.name().to_string_lossy().into_owned())
                            }
                            _ => Property::Value(format!("#{raw}")),
                        };
                    }
                    _ => {}
                }
            }
        }
        Err(e) => unnamed = Some(failure("DRM_IOCTL_MODE_OBJ_GETPROPERTIES", &e)),
    }
    if let Some(f) = unnamed {
        if matches!(hdr_output_metadata, Property::Absent) {
            hdr_output_metadata = Property::Failed(f.clone());
        }
        if matches!(colorspace, Property::Absent) {
            colorspace = Property::Failed(f);
        }
    }
    KmsConnector {
        connector_id: u32::from(handle),
        hdr_output_metadata,
        colorspace,
        crtc_mode: crtc_mode(card, handle),
    }
}

async fn colord_report() -> zbus::Result<Result<ColordReport, String>> {
    let conn = zbus::connection::Builder::system()
        .method_timeout(COLORD_TIMEOUT)
        .build()
        .await?;
    // Calling colord when it is not running would start it.
    let bus = zbus::fdo::DBusProxy::new(&conn).await?;
    if !bus.name_has_owner(COLORD.try_into()?).await? {
        return Ok(Err("colord is not running".into()));
    }
    let manager = zbus::Proxy::new(&conn, COLORD, "/org/freedesktop/ColorManager", COLORD).await?;
    let paths: Vec<OwnedObjectPath> = manager.call("GetDevicesByKind", &("display",)).await?;
    let mut devices = Vec::with_capacity(paths.len());
    for path in paths {
        let device = zbus::proxy::Builder::<zbus::Proxy<'_>>::new(&conn)
            .destination(COLORD)
            .path(path)
            .interface("org.freedesktop.ColorManager.Device")
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        let metadata: HashMap<String, String> = device.get_property("Metadata").await?;
        devices.push(ColordDevice {
            id: device.get_property("DeviceId").await?,
            owner: device.get_property("Owner").await?,
            metadata: metadata.into_iter().collect::<BTreeMap<_, _>>(),
        });
    }
    Ok(Ok(ColordReport {
        uid: rustix::process::getuid().as_raw(),
        devices,
    }))
}

impl LinuxNative for System {
    fn connectors(&mut self) -> Result<Vec<ConnectorReport>, DeviceError> {
        let dir = match fs::read_dir(SYSFS_DRM) {
            Ok(dir) => dir,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(unsupported(
                    "display enumeration",
                    format!("no {SYSFS_DRM}"),
                ));
            }
            Err(e) => return Err(failure("read_dir /sys/class/drm", &e).into()),
        };
        let mut reports = Vec::new();
        for entry in dir {
            let entry = entry.map_err(|e| failure("read_dir /sys/class/drm", &e))?;
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            // `card1-DP-6`; `card1` and `renderD128` are not connectors.
            let Some((card, name)) = file_name.split_once('-') else {
                continue;
            };
            if !card
                .strip_prefix("card")
                .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
            {
                continue;
            }
            reports.push(connector(&entry.path(), card, name));
        }
        reports.sort_by(|a, b| (&a.card, &a.connector).cmp(&(&b.card, &b.connector)));
        Ok(reports)
    }

    fn kms(&mut self, card: &str) -> Field<Vec<KmsConnector>> {
        let card = open_card(card)?;
        let resources = card
            .resource_handles()
            .map_err(|e| failure("DRM_IOCTL_MODE_GETRESOURCES", &e))?;
        Ok(resources
            .connectors()
            .iter()
            .map(|&h| kms_connector(&card, h))
            .collect())
    }

    fn colord(&mut self) -> Result<ColordReport, String> {
        zbus::block_on(colord_report()).unwrap_or_else(|e| Err(e.to_string()))
    }
}
