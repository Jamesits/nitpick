//! Optional local display process of the browser output: one full-screen
//! webview window with the nitpick page on the monitor that contains a
//! given point. It talks to the service only through the page.
//!
//! Exit status: 0 after the window closes or after `--list-monitors`, 1 if
//! the window cannot be shown, 2 for invalid arguments.

mod args;
mod monitor;

use std::process::ExitCode;

use clap::Parser;
use tauri::webview::NewWindowResponse;
use tauri::{App, PhysicalPosition, Url, WebviewUrl, WebviewWindowBuilder};

use args::Args;
use monitor::{Coordinates, Monitor};

/// WebView2 arguments. Wry passes the `--disable-features` list by
/// default; an explicit argument string replaces it, so it is repeated
/// here. With a forced scale factor of 1, `devicePixelRatio` is 1 at all
/// Windows scale settings, so one CSS pixel is one display pixel.
#[cfg(windows)]
const BROWSER_ARGS: &str = "--force-device-scale-factor=1 \
    --disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection";

fn main() -> ExitCode {
    let args = Args::parse();
    // No commands and no capabilities: a remote page has no IPC access.
    let app = match tauri::Builder::default().build(tauri::generate_context!()) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("error: cannot start the webview: {e}");
            return ExitCode::FAILURE;
        }
    };
    let monitors = match monitors(&app) {
        Ok(monitors) => monitors,
        Err(e) => {
            eprintln!("error: cannot read the monitors: {e}");
            return ExitCode::FAILURE;
        }
    };
    if args.list_monitors {
        for m in &monitors {
            println!(
                "{}\t{}\tscale {}",
                m.name,
                m.desktop_rect(Coordinates::NATIVE),
                m.scale
            );
        }
        return ExitCode::SUCCESS;
    }
    let (Some(url), Some(at)) = (args.url, args.at) else {
        unreachable!("clap requires --url and --at without --list-monitors");
    };
    let target = match monitor::select(&monitors, Coordinates::NATIVE, at) {
        Ok(i) => &monitors[i],
        Err(e) => {
            eprintln!("error: {e} {},{}", at.0, at.1);
            for m in &monitors {
                eprintln!("  {}\t{}", m.name, m.desktop_rect(Coordinates::NATIVE));
            }
            return ExitCode::FAILURE;
        }
    };
    if cfg!(target_os = "linux") {
        eprintln!("warning: WebKitGTK shows HDR images as SDR");
    }
    if let Err(e) = open_window(&app, url, target) {
        eprintln!("error: cannot open the window: {e}");
        return ExitCode::FAILURE;
    }
    // The event loop ends when the window closes.
    match app.run_return(|_, _| {}) {
        0 => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

fn monitors(app: &App) -> tauri::Result<Vec<Monitor>> {
    Ok(app
        .available_monitors()?
        .into_iter()
        .enumerate()
        .map(|(i, m)| Monitor {
            name: m.name().cloned().unwrap_or_else(|| format!("monitor {i}")),
            position: (m.position().x, m.position().y),
            size: (m.size().width, m.size().height),
            scale: m.scale_factor(),
        })
        .collect())
}

fn open_window(app: &App, url: Url, target: &Monitor) -> tauri::Result<()> {
    let page = url.clone();
    let builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
        .title("nitpick")
        .visible(false)
        .focused(true)
        .incognito(true)
        .on_navigation(move |to| args::allowed_navigation(&page, to))
        .on_new_window(|_, _| NewWindowResponse::Deny);
    #[cfg(windows)]
    let builder = builder.additional_browser_args(BROWSER_ARGS);
    let window = builder.build()?;
    let (x, y) = target.physical_centre();
    window.set_fullscreen_on_monitor(PhysicalPosition::new(x, y))?;
    window.show()?;
    window.set_focus()
}
