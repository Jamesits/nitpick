//! colord profile calls and profile files.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use nitpick_core::device::DeviceError;
use zbus::{Connection, ObjectPath, OwnedObjectPath};

use super::{DEVICE_INTERFACE, PROFILE_INTERFACE, System, connect, manager, proxy};
use crate::linux::{ColordProfile, LinuxProfileNative, Relation};

/// colord error names end in the error kind, after the interface name.
fn colord_error(api: &str, e: &zbus::Error) -> DeviceError {
    let native = |message: String| DeviceError::Native {
        api: api.into(),
        code: None,
        message,
    };
    match e {
        zbus::Error::InputOutput(io) if io.kind() == io::ErrorKind::TimedOut => {
            DeviceError::Timeout
        }
        zbus::Error::FDO(f) => match f.as_ref() {
            zbus::fdo::Error::AccessDenied(_) => {
                DeviceError::PermissionDenied(format!("{api}: {f}"))
            }
            zbus::fdo::Error::TimedOut(_) | zbus::fdo::Error::NoReply(_) => DeviceError::Timeout,
            _ => native(f.to_string()),
        },
        zbus::Error::MethodError(name, detail, _) => {
            let name = name.to_string();
            let detail = detail.clone().unwrap_or_default();
            match name.rsplit('.').next() {
                Some("FailedToAuthenticate") => {
                    DeviceError::PermissionDenied(format!("{api}: {detail}"))
                }
                Some("AlreadyExists" | "ProfileAlreadyAdded") => {
                    DeviceError::StateConflict(format!("{api}: {detail}"))
                }
                _ => native(format!("{name}: {detail}")),
            }
        }
        other => native(other.to_string()),
    }
}

fn is_not_found(e: &zbus::Error) -> bool {
    matches!(e, zbus::Error::MethodError(name, _, _) if name.to_string().ends_with(".NotFound"))
}

fn run<T>(api: &str, f: impl Future<Output = zbus::Result<T>>) -> Result<T, DeviceError> {
    zbus::block_on(f).map_err(|e| colord_error(api, &e))
}

fn object_path(path: &str) -> zbus::Result<ObjectPath<'_>> {
    ObjectPath::try_from(path)
}

async fn profile(conn: &Connection, path: &str) -> zbus::Result<ColordProfile> {
    let p = proxy(conn, path, PROFILE_INTERFACE).await?;
    Ok(ColordProfile {
        path: path.to_owned(),
        id: p.get_property("ProfileId").await?,
        filename: p.get_property("Filename").await?,
        owner: p.get_property("Owner").await?,
    })
}

async fn device_profiles(device: &str) -> zbus::Result<Vec<(ColordProfile, Relation)>> {
    let conn = connect().await?;
    let d = proxy(&conn, device, DEVICE_INTERFACE).await?;
    let paths: Vec<OwnedObjectPath> = d.get_property("Profiles").await?;
    let mut list = Vec::with_capacity(paths.len());
    for path in &paths {
        let relation: String = d.call("GetProfileRelation", &(path,)).await?;
        let relation = match relation.as_str() {
            "hard" => Relation::Hard,
            "soft" => Relation::Soft,
            _ => return Err(zbus::Error::InvalidReply),
        };
        list.push((profile(&conn, path.as_str()).await?, relation));
    }
    Ok(list)
}

async fn find_profile(id: &str) -> zbus::Result<Option<ColordProfile>> {
    let conn = connect().await?;
    let path: OwnedObjectPath = match manager(&conn).await?.call("FindProfileById", &(id,)).await {
        Ok(path) => path,
        Err(e) if is_not_found(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    profile(&conn, path.as_str()).await.map(Some)
}

/// colord reads the profile from the descriptor: it cannot read files in
/// a home directory. Compositors read `Filename`.
async fn create_profile(id: &str, file: &File, path: &str) -> zbus::Result<String> {
    let conn = connect().await?;
    let properties = HashMap::from([("Filename", path)]);
    let created: OwnedObjectPath = manager(&conn)
        .await?
        .call(
            "CreateProfileWithFd",
            &(id, "normal", zbus::Fd::from(file), properties),
        )
        .await?;
    Ok(created.to_string())
}

async fn add_profile(device: &str, profile: &str) -> zbus::Result<()> {
    let conn = connect().await?;
    proxy(&conn, device, DEVICE_INTERFACE)
        .await?
        .call("AddProfile", &("hard", object_path(profile)?))
        .await
}

/// A device method with one profile argument.
async fn device_call(method: &str, device: &str, profile: &str) -> zbus::Result<()> {
    let conn = connect().await?;
    proxy(&conn, device, DEVICE_INTERFACE)
        .await?
        .call(method, &(object_path(profile)?,))
        .await
}

async fn delete_profile(profile: &str) -> zbus::Result<()> {
    let conn = connect().await?;
    manager(&conn)
        .await?
        .call("DeleteProfile", &(object_path(profile)?,))
        .await
}

fn io_error(api: &str, path: &Path, e: &io::Error) -> DeviceError {
    match e.kind() {
        io::ErrorKind::PermissionDenied => {
            DeviceError::PermissionDenied(format!("{api} {}: {e}", path.display()))
        }
        io::ErrorKind::AlreadyExists => {
            DeviceError::StateConflict(format!("{} exists", path.display()))
        }
        _ => DeviceError::Native {
            api: api.into(),
            code: e.raw_os_error().map(i64::from),
            message: format!("{}: {e}", path.display()),
        },
    }
}

/// The XDG base directory rules: a relative `XDG_DATA_HOME` is ignored.
fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::home_dir().map(|h| h.join(".local/share")))
}

impl LinuxProfileNative for System {
    fn device_profiles(
        &mut self,
        device: &str,
    ) -> Result<Vec<(ColordProfile, Relation)>, DeviceError> {
        run("colord Device.Profiles", device_profiles(device))
    }

    fn find_profile(&mut self, id: &str) -> Result<Option<ColordProfile>, DeviceError> {
        run("colord FindProfileById", find_profile(id))
    }

    fn create_profile(&mut self, id: &str, file: &str) -> Result<String, DeviceError> {
        let path = Path::new(file);
        let opened = File::open(path).map_err(|e| io_error("open", path, &e))?;
        run(
            "colord CreateProfileWithFd",
            create_profile(id, &opened, file),
        )
    }

    fn add_profile(&mut self, device: &str, profile: &str) -> Result<(), DeviceError> {
        run("colord Device.AddProfile", add_profile(device, profile))
    }

    fn make_profile_default(&mut self, device: &str, profile: &str) -> Result<(), DeviceError> {
        run(
            "colord Device.MakeProfileDefault",
            device_call("MakeProfileDefault", device, profile),
        )
    }

    fn remove_profile(&mut self, device: &str, profile: &str) -> Result<(), DeviceError> {
        run(
            "colord Device.RemoveProfile",
            device_call("RemoveProfile", device, profile),
        )
    }

    fn delete_profile(&mut self, profile: &str) -> Result<(), DeviceError> {
        run("colord DeleteProfile", delete_profile(profile))
    }

    fn profile_dir(&mut self) -> Result<String, DeviceError> {
        let base = data_home().ok_or_else(|| DeviceError::Native {
            api: "home_dir".into(),
            code: None,
            message: "no XDG_DATA_HOME and no home directory".into(),
        })?;
        Ok(base.join("nitpick/icc").display().to_string())
    }

    fn read_file(&mut self, path: &str) -> Result<Option<Vec<u8>>, DeviceError> {
        match fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_error("read", Path::new(path), &e)),
        }
    }

    fn create_file(&mut self, path: &str, bytes: &[u8]) -> Result<(), DeviceError> {
        let path = Path::new(path);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| io_error("mkdir", dir, &e))?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| io_error("open", path, &e))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| io_error("write", path, &e))
    }

    fn remove_file(&mut self, path: &str) -> Result<(), DeviceError> {
        match fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                Err(io_error("unlink", Path::new(path), &e))
            }
            _ => Ok(()),
        }
    }
}
