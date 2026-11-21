//! QML-facing launcher object: pattern catalogue, display list, reference
//! tables, the browser service and the "Show pattern" request. The
//! lifecycle is in `session.rs`.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(QString, status)]
        /// JSON array: `[{driver, key, name, width, height}]`: the screens
        /// of the last `refreshDisplays`, then the browser clients.
        #[qproperty(QString, displays)]
        /// Changes when new device reports or browser client states
        /// arrive; the reference table depends on it.
        #[qproperty(i32, device_revision, cxx_name = "deviceRevision")]
        /// Saved measurement delay in ms; see `saveMeasurementDelay`.
        #[qproperty(i32, measurement_delay, cxx_name = "measurementDelay")]
        /// URL of the browser service; empty while it does not run.
        #[qproperty(QString, browser_url, cxx_name = "browserUrl")]
        /// Code for the browser that waits for authentication; empty
        /// otherwise.
        #[qproperty(QString, pairing_code, cxx_name = "pairingCode")]
        /// Browser client state for the user.
        #[qproperty(QString, browser_client, cxx_name = "browserClient")]
        /// Saved TCP port of the browser service; 0: the OS selects one.
        #[qproperty(i32, browser_port, cxx_name = "browserPort")]
        /// Saved interface address for remote displays.
        #[qproperty(QString, browser_address, cxx_name = "browserAddress")]
        type Launcher = super::LauncherRust;

        /// JSON object: `{patterns: [{id, label, description, encodings: [{id, label}]}],
        /// depths: [{id, label, browserLabel}], primaries: [{id, label}]}`.
        #[qinvokable]
        fn catalogue(&self) -> QString;

        /// Reference table for the native size of the display `display`
        /// of `driver` (a `driver` and a `key` of `displays`).
        #[qinvokable]
        #[cxx_name = "referenceTable"]
        fn reference_table(
            &self,
            pattern: &QString,
            encoding: &QString,
            depth: &QString,
            driver: &QString,
            display: &QString,
        ) -> QString;

        /// A display change: cancels the pending presentation, updates
        /// `displays` and reads the device state on a worker.
        #[qinvokable]
        #[cxx_name = "refreshDisplays"]
        fn refresh_displays(self: Pin<&mut Self>);

        /// Sets and saves `measurementDelay`. A negative value sets an
        /// error status and changes nothing.
        #[qinvokable]
        #[cxx_name = "saveMeasurementDelay"]
        fn save_measurement_delay(self: Pin<&mut Self>, ms: i32);

        /// `QRhi`: reads the device state of `display` on a worker,
        /// renders the pattern at the pattern area size of a new
        /// full-screen window and shows it. Browser: renders the pattern
        /// at the client viewport and sends it. Then waits `delay` ms.
        /// `metadata` is a JSON object, see `request_from_json`. `status`
        /// follows each step.
        #[qinvokable]
        #[cxx_name = "showPattern"]
        fn show_pattern(
            self: Pin<&mut Self>,
            pattern: &QString,
            encoding: &QString,
            depth: &QString,
            driver: &QString,
            display: &QString,
            metadata: &QString,
            delay: i32,
        );

        /// Starts the browser service, or starts it again. Without
        /// `remote`, it binds the loopback interface. With `remote`, it
        /// binds `address`, so that other devices on the network can
        /// connect. Saves `port`, and `address` with `remote`. A restart
        /// ends the client session.
        #[qinvokable]
        #[cxx_name = "startBrowser"]
        fn start_browser(self: Pin<&mut Self>, remote: bool, address: &QString, port: i32);

        /// JSON array of the interface addresses for remote displays.
        #[qinvokable]
        #[cxx_name = "interfaceAddresses"]
        fn interface_addresses(&self) -> QString;
    }

    impl cxx_qt::Threading for Launcher {}
}

use core::pin::Pin;
use std::sync::Arc;

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use nitpick_core::driver::{ContentLightLevel, DisplayId, Hdr10Request};
use nitpick_core::metadata::MasteringPrimaries;
use nitpick_core::output::SwapChainFormat;
use nitpick_core::pattern::PatternKind;
use nitpick_core::signal::{BitDepth, D65, Encoding, Format};
use serde_json::{Value, json};

use crate::delay::delay_from;
use crate::session::{Event, Session, ShowRequest};
use crate::settings::Settings;
use crate::{browser, display};

pub struct LauncherRust {
    status: QString,
    displays: QString,
    device_revision: i32,
    measurement_delay: i32,
    browser_url: QString,
    pairing_code: QString,
    browser_client: QString,
    browser_port: i32,
    browser_address: QString,
    settings: Settings,
    /// Created at the first request, which gives the Qt thread handle.
    session: Option<Session>,
}

impl Default for LauncherRust {
    fn default() -> Self {
        let settings = Settings::native();
        Self {
            status: QString::default(),
            displays: QString::default(),
            device_revision: 0,
            // Saved delays fit a Qt timer interval, see `delay_from`.
            measurement_delay: i32::try_from(settings.measurement_delay_ms()).unwrap_or(i32::MAX),
            browser_url: QString::default(),
            pairing_code: QString::default(),
            browser_client: QString::default(),
            browser_port: i32::from(settings.browser_port()),
            browser_address: QString::from(&settings.browser_address()),
            settings,
            session: None,
        }
    }
}

fn parse(
    pattern: &QString,
    encoding: &QString,
    depth: &QString,
) -> Result<(PatternKind, Format), String> {
    let encoding: Encoding = encoding.to_string().parse()?;
    let depth: BitDepth = depth.to_string().parse()?;
    Ok((pattern.to_string().parse()?, Format::new(encoding, depth)))
}

/// The display `key` of the driver `driver`: a `driver` and a `key` of
/// `displays`.
fn display_id(driver: &QString, key: &QString) -> Result<DisplayId, String> {
    let key = key.to_string();
    match driver.to_string().as_str() {
        display::DRIVER => Ok(display::display_for_key(key)),
        browser::DRIVER => Ok(DisplayId {
            driver: browser::DRIVER,
            key,
        }),
        other => Err(format!("unknown output driver `{other}`")),
    }
}

/// Parses the launcher JSON
/// `{enabled, primaries, maxMastering, minMastering, fromPattern, maxCll, maxFall}`
/// (luminances in cd/m²). `None` if metadata is off.
fn request_from_json(json: &str) -> Result<Option<Hdr10Request>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("metadata: {e}"))?;
    let number = |key: &str| {
        v[key]
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .ok_or_else(|| format!("metadata: `{key}` must be a number >= 0"))
    };
    if !v["enabled"].as_bool().unwrap_or(false) {
        return Ok(None);
    }
    let primaries: MasteringPrimaries = v["primaries"].as_str().unwrap_or_default().parse()?;
    let light_level = if v["fromPattern"].as_bool().unwrap_or(true) {
        ContentLightLevel::FromFrame
    } else {
        ContentLightLevel::Explicit {
            max_cll: number("maxCll")?,
            max_fall: number("maxFall")?,
        }
    };
    Ok(Some(Hdr10Request {
        primaries: primaries.primaries(),
        white: D65,
        max_mastering_nits: number("maxMastering")?,
        min_mastering_nits: number("minMastering")?,
        light_level,
    }))
}

fn id_labels<T: Copy>(
    items: &[T],
    id: impl Fn(T) -> &'static str,
    label: impl Fn(T) -> &'static str,
) -> Value {
    items
        .iter()
        .map(|&x| json!({"id": id(x), "label": label(x)}))
        .collect()
}

// QML invokables must be methods even when they need no state.
#[allow(clippy::unused_self)]
impl qobject::Launcher {
    pub fn catalogue(&self) -> QString {
        let patterns: Vec<_> = PatternKind::ALL
            .iter()
            .map(|p| {
                json!({
                    "id": p.id(),
                    "label": p.label(),
                    "description": p.description(),
                    "encodings": id_labels(p.encodings(), Encoding::id, Encoding::label),
                })
            })
            .collect();
        let depth_label = |d: BitDepth| match SwapChainFormat::for_depth(d) {
            SwapChainFormat::Hdr10 => "10-bit (HDR10 swap chain, exact codes)",
            SwapChainFormat::ScRgb => "12-bit (FP16 scRGB swap chain)",
        };
        let depths: Vec<_> = BitDepth::ALL
            .iter()
            .map(|&d| {
                json!({
                    "id": d.id(),
                    "label": depth_label(d),
                    "browserLabel": format!("{} (16-bit PNG)", d.label()),
                })
            })
            .collect();
        let value = json!({
            "patterns": patterns,
            "depths": depths,
            "primaries": id_labels(
                &MasteringPrimaries::ALL,
                MasteringPrimaries::id,
                MasteringPrimaries::label,
            ),
        });
        QString::from(&value.to_string())
    }

    pub fn reference_table(
        &self,
        pattern: &QString,
        encoding: &QString,
        depth: &QString,
        driver: &QString,
        display: &QString,
    ) -> QString {
        let parsed = parse(pattern, encoding, depth)
            .and_then(|(kind, format)| Ok((kind, format, display_id(driver, display)?)));
        let text = match (parsed, &self.session) {
            (Err(e), _) => e,
            (Ok(_), None) => "No display selected.".into(),
            (Ok((kind, format, id)), Some(s)) => s.reference_table(kind, format, &id),
        };
        QString::from(&text)
    }

    pub fn refresh_displays(mut self: Pin<&mut Self>) {
        let session = self.as_mut().session();
        if session.refresh().is_err() {
            // The nested event loop of `open` runs this; retry after it.
            let _ = self.qt_thread().queue(qobject::Launcher::refresh_displays);
            return;
        }
        self.update_displays();
    }

    fn update_displays(mut self: Pin<&mut Self>) {
        let list: Vec<Value> = self
            .as_mut()
            .session()
            .displays()
            .iter()
            .map(|d| {
                json!({
                    "driver": d.id.driver,
                    "key": d.id.key,
                    "name": d.name,
                    "width": d.size.0,
                    "height": d.size.1,
                })
            })
            .collect();
        self.set_displays(QString::from(&Value::from(list).to_string()));
    }

    pub fn show_pattern(
        mut self: Pin<&mut Self>,
        pattern: &QString,
        encoding: &QString,
        depth: &QString,
        driver: &QString,
        display: &QString,
        metadata: &QString,
        delay: i32,
    ) {
        let request = parse(pattern, encoding, depth).and_then(|(kind, format)| {
            Ok(ShowRequest {
                kind,
                format,
                metadata: request_from_json(&metadata.to_string())?,
                display: display_id(driver, display)?,
                delay_ms: delay_from(delay)?,
            })
        });
        match request {
            Ok(r) => self.as_mut().session().show(r),
            Err(e) => self.set_status(QString::from(&format!("Error: {e}"))),
        }
    }

    pub fn save_measurement_delay(mut self: Pin<&mut Self>, ms: i32) {
        let saved = delay_from(ms).and_then(|valid| {
            self.as_mut().set_measurement_delay(ms);
            self.settings
                .set_measurement_delay_ms(valid)
                .map_err(|e| format!("cannot save the measurement delay: {e}"))
        });
        if let Err(e) = saved {
            self.set_status(QString::from(&format!("Error: {e}")));
        }
    }

    pub fn start_browser(mut self: Pin<&mut Self>, remote: bool, address: &QString, port: i32) {
        let address = address.to_string();
        let result = u16::try_from(port)
            .map_err(|_| format!("the port must be in 0..=65535, not {port}"))
            .and_then(|port| Ok((port, browser::config(remote, &address, port)?)))
            .and_then(|(port, config)| {
                self.as_mut().set_browser_port(i32::from(port));
                if remote {
                    self.as_mut().set_browser_address(QString::from(&address));
                }
                let saved = self.browser_address().to_string();
                // A failed save does not stop the service.
                let save = self.settings.set_browser(port, &saved);
                self.as_mut().session().start_browser(&config)?;
                save.map_err(|e| format!("cannot save the browser settings: {e}"))
            });
        self.as_mut().update_browser();
        self.as_mut().update_displays();
        if let Err(e) = result {
            self.set_status(QString::from(&format!("Error: {e}")));
        }
    }

    pub fn interface_addresses(&self) -> QString {
        let list: Vec<String> = browser::interface_addresses()
            .iter()
            .map(ToString::to_string)
            .collect();
        QString::from(&Value::from(list).to_string())
    }

    fn update_browser(mut self: Pin<&mut Self>) {
        let view = self.as_mut().session().browser_view().clone();
        self.as_mut()
            .set_browser_url(QString::from(view.url.as_deref().unwrap_or_default()));
        self.as_mut().set_pairing_code(QString::from(
            view.pairing_code.as_deref().unwrap_or_default(),
        ));
        self.set_browser_client(QString::from(&view.client));
    }

    fn session(mut self: Pin<&mut Self>) -> &mut Session {
        if self.session.is_none() {
            let thread = self.qt_thread();
            let events = thread.clone();
            // Events go through the queue in order, so a later status
            // always arrives last. The launcher may be gone at exit.
            let session = Session::native(Arc::new(thread), move |event| {
                let _ = events.queue(move |mut launcher| match event {
                    Event::Status(text) => launcher.set_status(QString::from(&text)),
                    Event::Devices => {
                        let revision = launcher.device_revision().wrapping_add(1);
                        launcher.set_device_revision(revision);
                    }
                    Event::Displays => launcher.update_displays(),
                    Event::Browser => launcher.update_browser(),
                    Event::Service(service, event) => {
                        launcher.as_mut().session().on_service_event(service, event);
                    }
                });
            });
            self.as_mut().rust_mut().session = Some(session);
        }
        self.rust_mut()
            .get_mut()
            .session
            .as_mut()
            .expect("created above")
    }
}
