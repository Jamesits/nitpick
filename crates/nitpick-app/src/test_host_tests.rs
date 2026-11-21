//! The shared host contract on the executor of the Qt host: `gui_executor`
//! with a simulated event loop in place of the Qt event loop.

use std::ffi::OsString;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use nitpick_test_host::contract::{self, LocalExecutor};
use nitpick_test_host::{Environment, LocalFuture};

use crate::display::ffi::RendererInfo;
use crate::gui_executor::{self, Post};
use crate::qrhi_driver::tests::ChannelPost;

struct GuiLoop;

impl LocalExecutor for GuiLoop {
    fn run(&mut self, host: LocalFuture<()>, script: LocalFuture<()>) {
        let (tx, jobs) = mpsc::channel();
        let post: Arc<dyn Post> = Arc::new(ChannelPost(Mutex::new(tx)));
        let host = gui_executor::spawn_local(Arc::clone(&post), host);
        let script = gui_executor::spawn_local(post, script);
        // One job per event loop pass. No job left: both tasks wait.
        while !(host.is_finished() && script.is_finished()) {
            let job = jobs
                .try_recv()
                .expect("the scenario stopped making progress");
            job();
        }
    }
}

#[test]
fn shared_host_contract_on_the_qt_executor() {
    contract::run_all(&mut GuiLoop);
}

fn parse(args: &[&str]) -> Result<super::Options, String> {
    super::parse_options(args.iter().map(OsString::from))
}

#[test]
fn options_take_one_artifact_directory() {
    assert_eq!(parse(&[]), Ok(super::Options::default()));
    assert_eq!(
        parse(&["--artifacts", "out"]),
        Ok(super::Options {
            artifacts: Some("out".into())
        })
    );
    for args in [
        &["--artifacts"][..],
        &["--artifacts", "a", "--artifacts", "b"],
        &["--artifacts=out"],
        &["-platform", "offscreen"],
    ] {
        assert!(parse(args).is_err(), "{args:?}");
    }
}

fn renderer(vendor_id: u64, driver_version: &str) -> RendererInfo {
    RendererInfo {
        qt_version: "6.12.0".into(),
        backend: "D3D11".into(),
        device_name: "NVIDIA GeForce RTX 5090".into(),
        vendor_id,
        device_id: if vendor_id == 0 { 0 } else { 0x2b85 },
        driver_version: driver_version.into(),
    }
}

#[test]
fn environment_names_the_device_ids_and_driver_where_read() {
    let os = Some("Windows 10.0.26200 [64-bit]".to_owned());
    let e = super::environment(os.clone(), Some(&renderer(0x10de, "32.0.16.1692")));
    assert_eq!(e.os, os);
    assert_eq!(
        e.gpu.as_deref(),
        Some("NVIDIA GeForce RTX 5090 (vendor 0x10de, device 0x2b85), driver 32.0.16.1692")
    );
    assert_eq!(e.renderer.as_deref(), Some("Qt 6.12.0, QRhi D3D11"));
    assert_eq!((e.edid_sha256, e.profile_sha256), (None, None));
    assert_eq!(
        super::environment(None, Some(&renderer(0, "")))
            .gpu
            .as_deref(),
        Some("NVIDIA GeForce RTX 5090")
    );
    assert_eq!(
        super::environment(os.clone(), None),
        Environment {
            os,
            ..Environment::default()
        }
    );
}
