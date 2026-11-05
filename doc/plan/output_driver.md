# Plan: output drivers

Implement [the output driver design](../design/output_driver/overview.md) and [Output verification](../design/output_driver/testing.md). The design defines the contracts and pass criteria. This plan lists the remaining work in dependency order. Each item states its prerequisites and a completion check.

Only listed prerequisites block an item. Items without a `Requires` line have no prerequisites. Optional webview and IDD work does not block the core, browser or command-line host. Physical QRhi device checks remain in [the measurement plan](test_patterns.md); complete the relevant driver work before each check.

For code changes, run formatting, Clippy and the relevant nextest tests. Put Rust unit tests in separate `<file>_tests.rs` files. Keep device claims unverified until the required device checks pass.

### 1. Add independent output fixtures

TODO:

- Add the case manifest and locked `uv` reference generator in the locations defined by [Shared fixtures](../design/output_driver/testing.md#shared-fixtures). Generate code, luminance, gamut, mixed-channel, geometry and state vectors with independent f64 arithmetic. Profile fixtures are a separate item below.
- Verify reproducible generation, source and rounding records, and rejection of invalid cases. Rust tests must read the checked-in data without Python. Expected values must not come from production conversion code or Little CMS.

### 2. Add the common driver types and capability rules

TODO:

- Add `nitpick-core::driver` with the types, errors and borrowed non-`Send` `PresentFuture` in [Interface](../design/output_driver/overview.md#interface). Keep GUI, server and executor dependencies out of the core.
- Use a mock driver to check each `Yes`, `No` and `Unknown` rule. Reject unavailable depths, unsupported input and unsupported metadata. Accept interpretation and precision limits with warnings. Verify use through `dyn OutputDriver` on a local executor.

### 3. Calculate light levels from effective PQ samples

Requires: 1, 2.

TODO:

- Change `metadata::light_level` to consume normalized PQ samples supplied by a driver. Keep the shared maximum and frame-average calculation. Resolve `Hdr10Request` only when metadata is enabled; explicit values bypass the calculation.
- Verify a uniform 10-bit code-940 frame: QRhi full-range PQ gives about 4654.397 cd/m²; browser narrow-range PQ mapped to PNG peak gives 10000 cd/m². Check unequal channels and mixed pixels against independent expected values.

### 4. Extend QRhi integer packing checks

Requires: 1.

TODO:

- Extend `output_tests.rs` with independent `R10G10B10A2` expectations for every scalar code, asymmetric channels, alpha and row boundaries. Test equal input codes under all three encodings.
- Verify exact bytes and channel order. The scope encodings must retain the full-range PQ transport described in [Signalling](../design/output_driver/qt.md#signalling).

### 5. Test FP16 conversion and neighbour search independently

Requires: 1.

TODO:

- Add independent PQ, primary-matrix and 80 cd/m² vectors, including negative components. Extend the existing grey test with per-level BT.2111 results and fixed difficult cases for the deliberate ±2-neighbour search.
- Check native D65 matrices with primary and white vectors. Keep ICC D50 adaptation, assumed monitor white and rounded EDID coordinates out of the scRGB reference calculation.
- Verify the stored bits, tie behaviour, search bounds and residual code errors under [QRhi output](../design/output_driver/testing.md#qrhi-output). Do not use the production inverse or a total failure count as the sole expected result.

### 6. Report QRhi capabilities and reject unknown FP16 scales

Requires: 2, 4, 5.

TODO:

- Add structured path and mode reports to `pattern_window.cpp` and `display.rs`. Apply the [feature matrix](../design/output_driver/overview.md#feature-matrix). Restrict the scale assumptions and comments in `output.rs` to the supported scRGB path.
- Verify that macOS 12-bit output fails before conversion with the documented scale reason. Keep supported 10-bit HDR10 available. Test unsupported swap chains without an SDR fallback, Windows HDR-off reporting, and unknown mode fields on other platforms. Linux compositor scale remains unverified until capture and device qualification.
- Test Windows SDR WCG separately from HDR. Generic Advanced Color enablement or an active GPU colour conversion must not alone set `hdr_on` to true. Unknown calibration or dithering state must not produce an exact-code claim.

### 7. Resolve and inspect QRhi metadata

Requires: 3, 6.

TODO:

- Resolve automatic QRhi metadata from full-range PQ for every accepted encoding, including HLG scope input. Move native metadata packing behind an injectable bridge sink.
- Verify native field order, units, automatic and explicit values, disabled metadata and replacement. Use [QRhi metadata](../design/output_driver/qt.md#hdr10-static-metadata) as the contract. Keep the listed DXGI, Metal and display-effect questions open for device tests.

### 8. Acknowledge QRhi submission by frame ID

Requires: 6.

TODO:

- Replace success after `requestUpdate()` with a frame-ID callback after upload and successful `endFrame`. Report frame failures, closure and the queue-based timeout in [the QRhi design](../design/output_driver/qt.md#overview). Remove callback registrations on cancellation.
- With controlled render events, verify no early success, `beginFrame` and `endFrame` failures, timeout, close and two rapid replacements. Late work must not upload or complete a replacement request. Already submitted output can remain visible until replaced.

### 9. Implement asynchronous QrhiDriver

Requires: 2, 7, 8.

Open questions:

- Which existing executor integration can poll a local future and wake the Qt GUI thread? Check available libraries before adding a custom bridge.

TODO:

- Implement `OutputDriver` in `display.rs`. Run conversion in worker jobs with owned data; keep window and QRhi calls on the GUI thread. Resolve the future from bridge callbacks and wake the Rust task.
- Verify with controlled workers that Qt timers remain active during conversion and submission. Dropping the future must release its wait and callbacks without waiting for workers. Late results must not affect a new frame. Do not block or run a nested GUI event loop.

### 10. Make reference tables use output limits

Requires: 2, 5.

TODO:

- Supply selected-path limits to `pattern/catalogue.rs` and its reference table. Show interpretation warnings, FP16 precision marks and browser clipping marks from the relevant output contract.
- With mock path reports, verify that only the supported QRhi FP16 path has `*` marks. Browser nominal-range excursions must be marked as clipped at both depths. An unavailable path must not claim exact output.

### 11. Use the driver lifecycle in the Qt launcher

Requires: 9, 10.

TODO:

- Change `launcher.rs` and `qml/main.qml` to query before opening, render at the returned surface size, pass metadata requests and retain frame data until completion or cancellation. Remove eager metadata calculation from `metadata_from_json`.
- Verify depth selection, mode reporting and warnings. Keep QRhi narrow PQ and HLG selectable. Replacement, display change and closure must drop the future before reusing or closing the driver. Controls must remain active, and late results must not change the current status.

### 12. Add the Qt measurement delay

Requires: 11.

TODO:

- Add the saved delay control, Qt timer and status transitions from [Measurement delay](../design/output_driver/overview.md#measurement-delay). Keep warnings in the ready status.
- With a controlled clock, verify acknowledgement plus delay, zero delay, negative-input rejection and cancellation. Failed presentation must start no timer. Verify the default and persistence across launcher restart.

### 13. Add the independent browser service

Requires: 2.

TODO:

- Add `nitpick-browser` with embedded client assets, `axum`, a dedicated Tokio thread and the separate Rust service handle. Supply URL, pairing-code and client-state events, plus shutdown. Use the [host boundary](../design/output_driver/overview.md#host-and-dependency-boundary).
- Verify start, bind failure, event delivery and shutdown on a Rust executor. Build by package and inspect its dependency graph: Qt, Tauri and `nitpick-app` must be absent. Loopback is the default; a LAN bind requires an explicit host setting.

### 14. Encode browser PNG samples and colour tags

Requires: 1, 13.

Open questions:

- Does the selected `png` version expose the required colour and metadata chunks? Qualify its APIs; use `Writer::write_chunk` where needed.

TODO:

- Implement 16-bit RGB conversion and cICP output from [Code to PNG sample](../design/output_driver/browser.md#code-to-png-sample). Return clipping information with the encoded frame.
- Decode with an independent reader. Check every scalar code at both depths, inverse mapping in the nominal range, endpoint clipping, dimensions and channel order. Parse chunk placement and CRC independently. HLG must keep its HLG tag, with no competing colour tag.

### 15. Encode browser metadata from stored samples

Requires: 3, 14.

TODO:

- Resolve automatic PQ light levels after PNG clipping and rounding. Reject automatic HLG light levels and accept explicit values. Write PNG `mDCV` and `cLLI` separately from the existing SEI helpers.
- Run [browser metadata and light-level checks](../design/output_driver/testing.md#browser-output). Verify chunk widths, order and units, disabled omission, the code-940 case in item 3, and values affected by sample rounding. Browser use of these chunks remains unverified.

### 16. Implement browser session authentication

Requires: 13.

TODO:

- Implement [Authentication](../design/output_driver/browser.md#authentication): reserve one session before authentication, issue pairing codes and session tokens, restrict failed attempts, and revoke tokens on disconnect. Protect frame requests with bearer tokens and no-store responses.
- With a mock transport and clock, verify leading zeros, configured fixed strings, wrong or out-of-order messages, authentication timeout, second-client rejection, unauthorized image requests, revocation and reconnect. Tokens must remain absent from URLs.

### 17. Cache browser state and validate surfaces

Requires: 2, 16.

TODO:

- Accept valid authenticated `hello` messages. Implement synchronous `displays`, `query` and `open` from cached state. Apply browser mode and capability rules, including user-agent limits, viewport size and pixel mapping.
- Verify no client before valid `hello`, disconnect removal, unknown precision and metadata effect, SDR preview and layout warnings. Resize or pixel-ratio change must invalidate the surface and emit a host event. A stale surface must fail presentation without a network wait.

### 18. Implement browser presentation and cancellation

Requires: 15, 17.

TODO:

- Implement `BrowserDriver::present` with owned encoding jobs, frame storage, `show`, matching `applied` replies and task wakeups. Run acknowledgement timeouts on the server runtime. Bound pending work and release obsolete frame data.
- Verify acknowledgement ID and dimensions, client errors, timeout from `show`, disconnect, resize, close and cancellation under [Image requests and presentation](../design/output_driver/browser.md#image-requests-and-presentation). Late worker results and replies must not complete a new frame. Service events must remain available during a mutable driver borrow.

### 19. Add browser authentication and viewport controls

Requires: 16, 17.

TODO:

- Add the static page's code entry, in-memory token, user-activated full-screen button and `hello` reporting. Report changes to media queries, viewport, full-screen state and pixel ratio.
- Test authentication failures and reconnect. Verify state updates at pixel ratios 1 and 2, fractional scale, zoom and full-screen changes. The page must work in a normal browser without Tauri.

### 20. Add browser image application and acknowledgement

Requires: 18, 19.

TODO:

- Fetch PNGs with the bearer header. Manage blob URLs, decode, image layout and acknowledgement as defined by [Image requests and presentation](../design/output_driver/browser.md#image-requests-and-presentation). Apply the specified CSS pixel mapping.
- Test natural and layout sizes, HTTP and decode errors, object-URL release, and replacement during fetch or decode. A slow old frame must not replace a new image or the black close page. Verify that `applied` follows decode, DOM update and the required animation callbacks.

### 21. Add the command-line session host

Requires: 10, 18.

TODO:

- Add `nitpick-cli` with server, pattern and format options. Print URL, pairing codes, client state, output limits and status. Select clients by `DisplayId`, query before open, and render at the returned size. Await the borrowed future on a local Rust executor.
- With a mock driver and service, verify startup, selection, unsupported requests, replacement and exit. Process console input and service events while presentation is pending. Close only after releasing the future; keep the service and current frame alive until replacement or exit.

### 22. Add command-line measurement timing

Requires: 21.

TODO:

- Add the delay option and executor timer using the shared [measurement-delay contract](../design/output_driver/overview.md#measurement-delay). Print readiness with the driver warnings.
- Run the controlled-clock host checks for default, zero and invalid delay, failed presentation, replacement, viewport change, disconnect and exit. A stale timer must not report readiness for a new frame.

### 23. Verify the browser session without a window system

Requires: 22.

TODO:

- Run a real server and CLI with an authenticated mock client. Render with the core, fetch and independently decode the PNG, send `applied`, check readiness, replace the frame and shut down.
- Verify timeouts and disconnects during presentation and delay. Build by package without Qt, Tauri or a window system; check the dependency graph. Document the implemented CLI commands and flags in the design usage section.

### 24. Connect browser output to the Qt launcher

Requires: 12, 20, 23.

TODO:

- Add browser driver selection, service events, pairing display and the explicit remote-display setting. Populate clients from the service and use the existing host lifecycle and delay.
- Verify local and remote browser sessions, URL reporting, warnings and reference marks. A disconnect during delay must cancel readiness. A viewport change must cancel old work, reopen and render at the new size without blocking Qt controls.

### 25. Add the optional local webview process

Requires: 20.

TODO:

- Add `nitpick-webview` as a separate Tauri process with URL and screen-position arguments. Apply the platform rules in [Pixel mapping](../design/output_driver/browser.md#pixel-mapping), including the Windows scale argument.
- Verify monitor selection, full-screen layout, authentication and process exit. Report the Linux SDR limit. Build and run the browser service and CLI without this package.

### 26. Launch the optional webview from Qt

Requires: 24, 25.

TODO:

- Add the local-webview selection and process lifecycle in [Usage](../design/output_driver/browser.md#usage). Derive the monitor position from Qt; keep screen geometry out of the server.
- Verify launch when no session exists, pairing, the post-authentication `hello` timeout, process failure and screen changes. An existing client must not be replaced by another session.

### 27. Verify the real page in browser engines

Requires: 20, 23. Test the optional shell after 25.

TODO:

- Run the page checks in [Browser integration](../design/output_driver/testing.md#browser-integration) on each supported engine. Record engine and OS versions. Include resize, zoom, full-screen changes and cancellation during fetch or decode.
- Verify authenticated fetch, layout, acknowledgement and resource release. Use screenshots for geometry only. Keep HDR pixel and metadata-effect claims unverified until qualified capture or physical measurement.

### 28. Add the shared test-host protocol

Requires: 1, 12, 23.

TODO:

- Implement the JSON-lines requests and events in [Host boundary](../design/output_driver/testing.md#host-boundary). Use the real Qt adapter and CLI browser host, shared fixture IDs, frame IDs and surface generations. Keep capture outside `OutputDriver`.
- Drive both hosts from a test process. Verify query, open, show, close, error and readiness events, including replacement and malformed input. Run the shared host contract with both executors. An unavailable capture adapter must return a reason.

### 29. Add capture descriptors and result artifacts

Requires: 1, 28.

TODO:

- Add the raw-plane capture interface and result writer from [Host boundary](../design/output_driver/testing.md#host-boundary). Save source and frame identity, colour descriptors, raw API metadata, run settings and unmodified samples. Keep previews separate.
- With synthetic captures, verify row pitch, channel order, plane layout, unknown fields and artifact round trips. Return `pass`, `fail` or `unavailable` with a reason. Bound artifact queues and reject invalid descriptors without inventing a colour space.

### 30. Reject stale captures and sample valid regions

Requires: 29.

TODO:

- Add the binary frame marker, optional heartbeat, bounded capture wait and region selection from [Timing and frame identity](../design/output_driver/testing.md#timing-and-frame-identity).
- Replay synthetic frame streams. Verify timestamps, current marker, three matching frames, deadline, resize and display-state changes. Test inset patch sampling separately from one-pixel geometry. Do not resize a capture to pass comparison.

### 31. Add comparison metrics and fixed limits

Requires: 1, 29.

TODO:

- Implement component errors and the independent [reference metric](../design/output_driver/testing.md#reference-metric). Store per-case numeric bounds and the proposed perceptual target from [Pass criteria](../design/output_driver/testing.md#pass-criteria).
- Verify analytical cases, primary vectors and the BT.2124 worked example. Reject nonfinite values and invalid metric domains. Preserve signed values; require an explicit HLG viewing model. Check that FP16 search error is counted once and that a failed case cannot widen its limits.

### 32. Verify QRhi offscreen draw readback

Requires: 4, 5, 9, 29.

TODO:

- Add readback after the production upload, shader and pipeline draw to a supported offscreen target. Normalize only the documented row pitch and image origin.
- On D3D11, Metal and Vulkan, compare exact stored samples and geometry against CPU output. Include asymmetric corners, odd widths, channel ramps and alpha. A software device result covers arithmetic only, as defined by [QRhi readback](../design/output_driver/testing.md#qrhi-readback).

### 33. Verify QRhi swap-chain setup and failure handling

Requires: 7, 12, 28, 32.

TODO:

- Record native swap-chain format and colour space. Read back the actual swap-chain image after draw where supported. Keep an unavailable swap-chain readback distinct from an offscreen pass.
- On each backend, verify supported depths, geometry, metadata sink values, delayed submission, out-of-date surfaces, device loss, close during upload and rapid replacement. Success must name the submitted frame; an upload-texture dump cannot pass this check.

### 34. Add display-state restoration and crash recovery

Requires: 29.

TODO:

- Add saved display settings, profile associations and a durable recovery record before any test changes them. Follow [Usage and restoration](../design/output_driver/testing.md#usage-and-restoration). Remove only resources created by the run.
- Include legacy gamma ramps, external calibration loaders, GPU gamut clamps and dithering in the recorded setup. Establish a baseline without competing transforms. Recheck it after HDR, topology or resume events, because background tools can reapply calibration. Record unreadable state as unknown; do not assume that an unsupported NVAPI call cleared it.
- With an injected settings backend, verify restoration on success, failure, timeout and cancellation. Simulate a crash and recover on the next run. A restoration mismatch must fail the run. Test this before using profile or mode changes on a display.

### 35. Add Windows FP16 capture

Requires: 29, 30.

TODO:

- Add the WGC window adapter in [Platform capture adapters](../design/output_driver/testing.md#platform-capture-adapters). Keep FP16 through storage and preserve content size, timestamps and raw metadata.
- Verify negative and greater-than-one samples, row layout, resize and window identity. Report permission or API failure with a reason. Record the submitted colour tag; do not infer the capture colour contract from FP16 storage.

### 36. Add macOS HDR capture

Requires: 29, 30.

Open questions:

- Which ScreenCaptureKit preset and high-precision format preserve the required signal on the selected macOS runner? Absolute luminance remains unverified until qualification.

TODO:

- Add the selected HDR stream adapter and retain pixel-buffer attachments, range and preset. Handle permission failure and any explicit YCbCr conversion as specified by [Platform capture adapters](../design/output_driver/testing.md#platform-capture-adapters).
- Verify buffer layout, timestamps and resize handling. Qualify on supported hardware before numeric HDR comparisons. Do not assign the Windows scRGB scale to generic EDR values or enable QRhi 12-bit output.

### 37. Add Linux capture with an explicit colour contract

Requires: 29, 30.

Open questions:

- Can the pinned compositor and portal backend provide high-precision samples and all required colour fields? Its capture stage and absolute scale remain unverified.

TODO:

- Add the ScreenCast portal/PipeWire adapter, or the required compositor adapter, from [Platform capture adapters](../design/output_driver/testing.md#platform-capture-adapters). Record negotiated samples and colour state.
- Verify layout, timestamps, resize and missing-field handling. Missing transfer, primaries, range or scale must make absolute HDR comparison unavailable. Keep X11 screenshots limited to geometry.

### 38. Add known ICC and MHC2 profile fixtures

Requires: 1.

TODO:

- Add complete display ICC fixtures for identity, matrix-only, LUT-only and combined calibration. Define matrix, curve and luminance expectations before serialization. Keep other display metadata equal across transform comparisons.
- Store both intended values and expected signed 15.16 values after serialization. State the rounding rule and test positive and negative half steps. Add a red/green exchange whose XYZ matrix is derived from its intended RGB action in the native sRGB/D65 basis.
- Give MHC2 minimum and peak display luminance, ICC `lumi` full-frame luminance and frame MaxCLL/MaxFALL distinct values in the manifests.
- Add the malformed-input and curve-boundary cases in [MHC2 profile adapter](../design/output_driver/testing.md#mhc2-profile-adapter). Verify fixture manifests, hashes and reproducible bytes. Keep profile extraction checks separate from later Windows installation checks.

### 39. Add and qualify the Little CMS profile adapter

Requires: 38.

Open questions:

- Which pinned `lcms2` binding and native Little CMS build pass every required fixture on all three operating systems? Native build settings and versions must be recorded separately.

TODO:

- Add the test-support adapter under [Colour library boundary](../design/output_driver/testing.md#colour-library-boundary). Use library tag access and return owned Rust data. Keep native handles, GUI dependencies and Windows APIs out of the model interface.
- Verify extracted values against manifests, including identity fields, signed values, bounds failures and unsupported LUT counts. Missing or unreadable tags must not silently become identity. Qualify the exact native build; resolve valid-profile failures before enabling the case.
- Test relocated tag data and internal offsets; do not assume a fixed file offset or contiguous matrix and LUT storage. Cover zero matrix offset, zero LUT count, two-entry identity curves and a nonzero ignored fourth matrix column. Compare decoded values to the serialized expectations from item 38.

### 40. Verify conventional ICC transforms separately

Requires: 39.

TODO:

- Add the adapter's conventional ICC transform tests with explicit formats, intent, adaptation and black-point compensation settings. Use independent identity, transfer-curve and primary-conversion expectations.
- Use profiles with known white points and `chad` values to distinguish native XYZ from D50 PCS values. Verify that adaptation occurs only where requested. Keep any `vcgt` loading separate from ordinary ICC pixel conversion; do not apply calibration twice.
- Verify numeric bounds and record all options and library versions. Keep these results separate from MHC2 model and Windows calibration results. Account for the library's f32 stage precision where stages are used.

### 41. Add the explicit HDR display model

Requires: 31, 39.

TODO:

- Implement [Model order and domain](../design/output_driver/testing.md#model-order-and-domain) as portable test support. Require source and wire spaces and luminance scales. Keep conventional ICC adaptation separate from MHC2 evaluation.
- Apply the extracted MHC2 LUT directly after wire transfer encoding. Do not add an inverse ICC TRC or `vcgt` stage to a LUT that already contains that calibration. Keep NVAPI's SDR LUT construction and its 8-bit sample optimization out of this model.
- Verify identity, XYZ gain, asymmetric matrices, channel LUTs and an order-sensitive combined case against independent vectors. Check knots, interpolation, clipping and quantization; retain pre-quantization floats. Report these as ideal model results, not GPU behaviour.
- Use the RGB exchange fixture to detect a wrong matrix basis or transpose. Test sRGB and gamma 2.2 as distinct SDR transfers, including near-black values. Keep the runner's actual SDR transfer unverified until measured or established by its wire contract.

### 42. Qualify capture adapters and compare output

Requires: 27, 30, 31, 33, 34, plus the applicable adapter from 35–37. Windows profile comparisons also require 39, 41.

TODO:

- Run [Capture reference and qualification](../design/output_driver/testing.md#capture-reference-and-qualification) per OS, backend and browser engine. Include black, HDR neutrals, wide-gamut colours, geometry and a separate tagged sRGB control. Save accepted frame timestamps and encoded browser PNGs.
- Compare QRhi against submitted values and ideal colour separately. On Windows, compare calibration profiles with equal display metadata and restore settings. Do not use MHC2 as a portable calibration test. Record capture conversion and colour scale; unknown contracts must not pass absolute HDR checks. Windows SDR-white behaviour and Linux scRGB scale need measured results.
- Record whether calibration changes capture. Leave the calibration boundary unverified until the IDD or physical comparison is available. Capture can change composition; the physical check must also run without capture.

### 43. Model IDD callback state without an installed driver

Requires: 41.

TODO:

- Add portable callback records and state replay from [Instrumented Windows IDD](../design/output_driver/testing.md#instrumented-windows-idd). Model enable flags, the scalar, affine matrix, LUT, transform generation and HDR metadata selection.
- Verify disabled stages, non-unit scalar, nonzero affine terms, replacement, reset and new/previous/default metadata. Reject previous metadata before any exists. Keep the active IDD affine column distinct from the ignored profile matrix column; avoid scaling HDR twice with SDR white.

### 44. Qualify the optional console IDD runner

Open questions:

- Which pinned console IDD revision supports the required surface, gamma and metadata callbacks? Build, signing and runtime behaviour remain unverified.

TODO:

- Audit and build the candidate in [Instrumented Windows IDD](../design/output_driver/testing.md#instrumented-windows-idd). Provision signing manually on a dedicated runner. Check runtime API availability, HDR modes and display state; the harness must not change boot security.
- Record the revision, build and qualification result. An advertised HDR EDID alone must not qualify the runner. Repeat qualification after a driver or OS update. Use this trial and the capture trial before estimating the remaining lab work.

### 45. Instrument IDD surfaces and transform generations

Requires: 29, 43, 44.

TODO:

- Record acquired surface bytes, colour metadata, committed wire mode, gamma callbacks and metadata selection. Copy surfaces before ownership expires. Capture the IDD's own transformed output before its encoder.
- Verify that each output uses one recorded transform generation, including callback changes during processing. Replay captured input and state against the portable model. A CPU recomputation must not substitute for the driver's transformed artifact.

### 46. Verify Windows profile loading through the IDD

Requires: 34, 39, 41, 45.

Open questions:

- Does the selected runner load each new profile without stale calibration? MHC2 activation and legacy gamma-ramp loader settings are separate; qualify the actual transition on this runner.

TODO:

- Install the known profiles on the qualified runner. Test HDR and SDR WCG separately, holding other display metadata and controls fixed. Record actual source and wire spaces.
- Check activation through the received transform generation and its effect. A successful profile association or a calibration-loader setting is not proof of activation. Do not use an NVAPI colour-conversion call as an MHC2 loading fallback. Test a missing or stale transform as a qualification failure.
- Compare effective matrix action, sampled curves, raw surfaces and driver output under [Instrumented Windows IDD](../design/output_driver/testing.md#instrumented-windows-idd). Allow recorded callback representation and resampling; reject unexplained transforms. Verify restoration and retain evidence. A pass applies only to that IDD path.

### 47. Add cross-platform execution gates

Requires: 23, 28, 31, 39–41. Add device jobs after their qualification items pass.

TODO:

- Add the ordinary CI jobs and separate GPU, HDR, calibration and release jobs from [Run matrix](../design/output_driver/testing.md#run-matrix). Run core, driver, fixture and mock protocol checks on Windows, Linux and macOS without a virtual display.
- Verify routing and result aggregation with injected pass, fail and unavailable results. A required device losing qualification must fail; an optional unavailable device must not count as HDR coverage. Record conversion, encoding, acknowledgement, capture and settle times separately. Fail bounded-wait and queue-growth checks independently of pixel results.

### 48. Record physical output qualification

Requires: the relevant host and driver checks, 34, and qualified instruments. Use 42 only for comparisons that include OS capture.

TODO:

- Run the pending QRhi checks in [the measurement plan](test_patterns.md). Add release measurements for browser output, including a remote display, using [Pass criteria](../design/output_driver/testing.md#pass-criteria) and [Usage and restoration](../design/output_driver/testing.md#usage-and-restoration).
- Record wire samples and metadata where a cable exists, repeated probe readings, quantized references, uncertainty, display controls and delay. Check FP16 exceptions and above-peak tone mapping separately. A server capture cannot qualify a remote browser's light output.
- Record actual wire depth and dithering state separately. Check repeated wire samples for temporal changes; one frame cannot establish code preservation. Keep gamma-ramp, GPU-clamp and MHC2 effects isolated under the baseline from item 34.
- Store results at their measured boundary, verify restoration, and update only the claims supported by those results. Remove completed plan items without renumbering the remaining items.
