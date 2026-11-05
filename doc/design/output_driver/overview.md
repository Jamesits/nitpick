# Output drivers

## Overview

An output driver shows a rendered test pattern on one display. This design defines two drivers:
- **QRhi** (`qrhi`): a full-screen Qt window with an HDR swap chain on a local screen. See [qt.md](qt.md).
- **Browser** (`browser`): a web page that shows the pattern as an HDR image. The page runs in a local Tauri webview (for tests) or in a remote browser, for example Safari on an iPad. See [browser.md](browser.md).

The compatibility API is one Rust trait, `OutputDriver`, in `nitpick-core` (module `driver`). It has no Qt, Tauri or network dependency. The launcher uses only this trait. The drivers do not have the same features. The API reports each feature as supported, not supported or unknown, so that the launcher can disable options and the reference table can show the limits.

**Not implemented:** the common trait and browser driver. The existing QRhi bridge needs the changes in [the output driver plan](../../plan/output_driver.md).

```mermaid
graph LR
    QML[qml/main.qml] --> L[launcher.rs]
    L -->|render, reference_table| C[nitpick-core<br/>pattern]
    L -->|displays, query, open, present| API[nitpick-core::driver<br/>OutputDriver]
    API --> Q[QrhiDriver<br/>nitpick-app/display.rs]
    API --> B[BrowserDriver<br/>nitpick-browser]
    Q --> W[pattern_window.cpp<br/>QRhi swap chain]
    B -->|HTTP + WebSocket| P[web client<br/>img element]
    P --- T[nitpick-webview<br/>Tauri, local]
    P --- R[remote browser<br/>iPad Safari]
```

## Interface

Pseudo-code (Rust). The launcher gives code values and metadata settings. The driver converts pixels, calculates automatic light levels and encodes metadata.

```rust
use std::future::Future;
use std::pin::Pin;

pub enum Support {
    Yes,
    No(String),      // reason, shown in the launcher
    Unknown(String), // the driver cannot detect it; reason
}

pub struct DisplayId { pub driver: &'static str, pub key: String }

pub struct DisplayInfo {
    pub id: DisplayId,
    pub name: String,             // screen name, or client name and address
    pub size: (usize, usize),     // native pixels; the size of the reference table
}

/// Features available with the current display and OS settings.
pub struct Capabilities {
    pub depths: Vec<(BitDepth, DepthSupport)>,
    pub encodings: Vec<(Encoding, EncodingSupport)>,
    pub hdr_metadata: Support,                // ST 2086 + CTA-861.3 reach the display
    pub pixel_exact: Support,                 // one pattern pixel = one display pixel
}

pub struct DepthSupport {
    pub available: Support, // the driver can open this output path
    pub path: String,      // for example "HDR10 swap chain", "16-bit PNG, cICP PQ"
    pub exact: Support,    // the codes reach the wire or the panel without change
}

pub struct EncodingSupport {
    pub input: Support,           // the driver accepts frames in this encoding
    pub interpretation: Support,  // the output preserves the intended transfer and range
    pub auto_light_level: Support, // the driver can calculate absolute content light levels
}

/// Current settings of the OS, the driver and the browser. Read-only.
/// `None`: the driver cannot read the value.
pub struct ModeSettings {
    pub hdr_on: Option<bool>,
    pub gamut: Option<Gamut>,           // srgb, p3, rec2020
    pub wire_depth: Option<u8>,         // bits per component
    pub peak_nits: Option<f64>,
    pub min_nits: Option<f64>,
    pub sdr_white_nits: Option<f64>,
    pub pixel_ratio: f64,               // display pixels per layout pixel
    pub description: String,
}

pub struct Query { pub capabilities: Capabilities, pub mode: ModeSettings }

pub struct Surface { pub display: DisplayId, pub size: (usize, usize), pub description: String }

pub enum ContentLightLevel {
    FromFrame,
    Explicit { max_cll: f64, max_fall: f64 }, // cd/m²
}

pub struct Hdr10Request {
    pub primaries: [[f64; 2]; 3], // CIE xy, red, green, blue
    pub white: [f64; 2],
    pub max_mastering_nits: f64,
    pub min_mastering_nits: f64,
    pub light_level: ContentLightLevel,
}

pub struct Frame<'a> {
    pub image: &'a Image,                     // codes, size = surface size
    pub format: Format,
    pub metadata: Option<&'a Hdr10Request>,   // None: no metadata
}

pub enum OutputError {
    Unsupported { feature: String, reason: String },
    Disconnected,
    Timeout,
    Platform(String),
}

// No Send bound: QRhi must stay on the Qt GUI thread.
pub type PresentFuture<'a> =
    Pin<Box<dyn Future<Output = Result<String, OutputError>> + 'a>>;

pub trait OutputDriver {
    fn id(&self) -> &'static str;
    fn displays(&mut self) -> Vec<DisplayInfo>;
    fn query(&mut self, display: &DisplayId) -> Result<Query, OutputError>;
    /// Prepares a full-screen surface for `depth` and gives its pixel size.
    /// The launcher renders the pattern at this size.
    fn open(&mut self, display: &DisplayId, depth: BitDepth) -> Result<Surface, OutputError>;
    /// Resolves after driver acknowledgement, or an error. Does not block the GUI thread.
    /// Returns a status line. Physical presentation is not confirmed.
    fn present<'a>(
        &'a mut self, surface: &'a Surface, frame: &'a Frame<'a>,
    ) -> PresentFuture<'a>;
    fn close(&mut self, display: &DisplayId);
}
```

Rules:
- `open` rejects a depth whose `available` is `No`. `present` rejects an encoding whose `input` is `No`, or metadata whose `hdr_metadata` is `No`. `FromFrame` also requires `auto_light_level` other than `No`.
- `interpretation = No` does not reject the frame. The launcher keeps the encoding selectable and shows the reason in the status and reference table. This permits QRhi scope use with HLG codes and PQ signalling.
- `exact = No` and `pixel_exact = No` also produce warnings. They do not disable approximate output. For any selected feature with `Unknown`, continue with a warning.
- The driver resolves `Hdr10Request` to `Hdr10Metadata`. See [content light level calculation](../test_patterns.md#hdr10-static-metadata) for the shared calculation and the HLG rule.
- The launcher calls `query` again before each `open`, because the user can change the OS settings (HDR on/off, browser zoom) at any time.

### Asynchronous presentation

The launcher polls `PresentFuture` through an executor integrated with the Qt event loop. The future has no `Send` bound. Qt window and QRhi calls stay on the GUI thread. Waiting for render events, network replies or timeouts must yield to that loop; do not use `block_on`, a sleep or a nested event loop for `present`.

CPU pixel conversion and PNG encoding use worker jobs with owned input data. The browser server continues to use its own tokio thread. Workers and the server notify the GUI executor when a result is ready. They do not access Qt objects. The future owns its pending jobs and callback registrations; the caller retains the frame data until the future is complete or dropped.

Only one presentation may be pending per driver. A new request cancels the old one before it borrows the driver again. Dropping the future cancels its wait, timer and callbacks without waiting for worker jobs. Work that has already reached the GPU or browser can still finish; cancellation does not undo that output. Use a unique frame ID for each request and discard late worker results and acknowledgements. A disconnect or a closed surface completes a pending future with `Disconnected`.

The launcher shows "Submitting pattern" until the future resolves. Its controls remain active. A new frame, a close request or a display change cancels the pending future and the measurement timer. Call `close` after releasing the future's borrow. `query` and `open` remain synchronous; asynchronous surface creation is outside this change.

### Measurement delay

`present` confirms driver acknowledgement only. QRhi acknowledges a successful submission of the frame. The browser acknowledges client decode and layout. Neither reports physical scanout or stable display light.

The launcher has a saved "Measurement delay (ms)" setting. It accepts a non-negative integer. The default is 1000 ms. **Unverified:** this default is sufficient for a particular display or probe.

After the `present` future succeeds, the launcher starts a Qt timer for this delay and shows "Waiting for measurement delay". Qt continues to process events. When the timer expires, the status is "Ready after delay: N ms", with the driver warnings. A zero delay adds no wait. The driver acknowledgement timeout excludes this delay.

A new frame, a closed surface, a disconnect or a change of the selected display cancels the old timer. A failed `present` does not start it. Each timer belongs to one frame, so an old timer cannot mark a new frame ready.

Before a measurement, set the delay for the display and probe, show the pattern, and wait for "Ready after delay". Increase the delay if repeated readings still change. The delay is a timing allowance, not proof of physical presentation.

## Feature matrix

| Feature | QRhi | Browser |
|---|---|---|
| Displays | local screens (`QGuiApplication::screens()`) | connected web clients |
| 10-bit | HDR10 swap chain, exact if the compositor does not change codes | 16-bit PNG, cICP; exact: `Unknown` |
| 12-bit | FP16 scRGB where the luminance scale is known; macOS: `available = No`; see [Swap chain output](qt.md#swap-chain-output) | 16-bit PNG, cICP; exact: `Unknown` |
| `pq-narrow` / `pq-full` | `input = Yes`; written as full-range PQ codes; narrow interpretation: `No` | full-range PQ PNG; narrow input is rescaled; see [Code to PNG sample](browser.md#code-to-png-sample) |
| `hlg-narrow` | `input = Yes`, `interpretation = No`; scope use only | full-range HLG PNG; narrow input is rescaled; interpretation: `Unknown` |
| Automatic light levels | full-range PQ interpretation of all input encodings | PQ after PNG sample conversion; HLG: `No` |
| HDR10 metadata | Windows, Linux, macOS (see [HDR10 static metadata](qt.md#hdr10-static-metadata)) | embedded in the PNG (`mDCV`, `cLLI`); effect `Unknown` |
| `hdr_on` | Windows: DXGI output colour space | CSS `(dynamic-range: high)` |
| `gamut` | Windows: DXGI output primaries | CSS `(color-gamut: …)` |
| `wire_depth` | Windows: DXGI `BitsPerColor` | `None` |
| `peak_nits`, `min_nits` | Windows: DXGI (EDID) | `None` |
| `sdr_white_nits` | `None` | `None` |
| `pixel_exact` | `Yes` (swap chain size = pattern size) | `Yes` only for an integer `pixel_ratio` and a layout that fills the viewport |

Linux and macOS QRhi give `None` for the mode settings that the table gives for Windows only.

## Technical decisions

| Decision | Options | Selected | Reason |
|---|---|---|---|
| API location | `nitpick-app`; `nitpick-core`; new crate | `nitpick-core` | The trait uses only core types (`Image`, `Format`, `Hdr10Request`). The browser driver crate does not depend on Qt. |
| API input | pixels in the swap chain format; code values | code values (`Image`) | Each driver has a different pixel format. The conversion is a driver detail. |
| Unsupported features | one common subset; tri-state report per feature | separate input and interpretation reports | Scope use can require codes whose signalling differs from their nominal encoding. `Unknown` is necessary, because a browser cannot report what the OS compositor does. |
| Automatic metadata | launcher assumes PQ; driver supplies the effective signal to the shared calculation | driver supplies the effective signal | Pixel conversion can change range, transfer and clipping. The metadata must describe the signal sent. |
| Call model | synchronous; asynchronous | asynchronous `present`, synchronous `query` and `open` | GPU submission and remote clients can be slow. Waiting must leave the Qt event loop active. |
| Future type | native async trait method; boxed future | boxed future without `Send` | Supports `dyn OutputDriver` and GUI-thread-only QRhi operations. The core API needs no executor or GUI dependency. |
| Measurement timing | assumed physical completion; fixed delay; user delay after acknowledgement | user delay in the launcher | Display and probe timing differ. A Qt timer keeps the UI active during the delay. |

## Testing

- Unit tests with a mock driver: reject `available = No` and `input = No`; keep `input = Yes` selectable when `interpretation = No`; show warnings for `No` interpretation or precision and for `Unknown`; map each `OutputError` to a status line.
- Async tests: delayed acknowledgement leaves Qt timers and controls active. Dropping the future releases callbacks and permits a new request. Late results cannot complete that request. A closed surface or disconnected client returns `Disconnected`; missing acknowledgement returns `Timeout`.
- Test the delay with a controlled clock: readiness follows acknowledgement plus the selected delay; zero adds no wait; errors start no timer; replacement, closure and disconnect cancel the old timer. Verify that Qt still processes events during the delay.
- Each driver: `query` of a display with HDR off gives `hdr_on = Some(false)` (QRhi on Windows, browser) and the launcher shows it.

## Future enhancements

- A driver for a hardware pattern generator or a video output card, through the same trait.
