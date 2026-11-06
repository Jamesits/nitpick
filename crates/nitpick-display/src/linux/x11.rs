//! RandR output reports of the X server in `DISPLAY`, for the
//! [`ScreenSource`] of an X11 screen.
//!
//! This is a second connection, not the host's own: Qt does not give the
//! raw EDID. It reaches the same server only if the host uses `DISPLAY`;
//! Qt's `-display` option is not followed.

use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

use super::ScreenSource;

/// Xwayland sets this property on each RandR output; other X servers do
/// not.
const EMULATION: &[u8] = b"RANDR Emulation";
const EDID: &[u8] = b"EDID";
/// In 32-bit units: 1024 bytes. Larger than an EDID with 7 extension
/// blocks.
const EDID_LONGS: u32 = 256;

/// The source of the RandR output `name`.
///
/// # Errors
/// No X server, no RandR output with this name, or a failed request.
pub fn screen_source(name: &str) -> Result<ScreenSource, String> {
    read(name).map_err(|e| format!("RandR output {name}: {e}"))
}

fn read(name: &str) -> Result<ScreenSource, Box<dyn std::error::Error>> {
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    let resources = conn.randr_get_screen_resources_current(root)?.reply()?;
    for output in resources.outputs {
        let info = conn
            .randr_get_output_info(output, resources.config_timestamp)?
            .reply()?;
        if info.name != name.as_bytes() {
            continue;
        }
        let properties = conn.randr_list_output_properties(output)?.reply()?.atoms;
        let has = |atom: Option<u32>| atom.is_some_and(|a| properties.contains(&a));
        if has(atom(&conn, EMULATION)?) {
            return Ok(ScreenSource::Xwayland);
        }
        let edid = match atom(&conn, EDID)? {
            Some(a) if has(Some(a)) => Some(
                conn.randr_get_output_property(
                    output,
                    a,
                    AtomEnum::ANY,
                    0,
                    EDID_LONGS,
                    false,
                    false,
                )?
                .reply()?
                .data,
            ),
            _ => None,
        };
        return Ok(ScreenSource::X11 { edid });
    }
    Err("no such output".into())
}

/// The atom of `name`, without creating it. `None` if no client created
/// it; then no output has the property.
fn atom(conn: &RustConnection, name: &[u8]) -> Result<Option<u32>, Box<dyn std::error::Error>> {
    let atom = conn.intern_atom(true, name)?.reply()?.atom;
    Ok((atom != u32::from(AtomEnum::NONE)).then_some(atom))
}
