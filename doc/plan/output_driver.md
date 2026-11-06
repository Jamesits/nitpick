# Plan: output drivers

Implement [the output driver design](../design/output_driver/overview.md), [display device operations](../design/display_device.md) and [Output verification](../design/output_driver/testing.md). The design defines the contracts and pass criteria. This plan lists the remaining work in dependency order. Each item states its prerequisites and a completion check.

Only listed prerequisites block an item. Items without a `Requires` line have no prerequisites. Optional webview and IDD work does not block the core, browser or command-line host. Physical QRhi device checks remain in [the measurement plan](test_patterns.md); complete the relevant driver work before each check.

For code changes, run formatting, Clippy and the relevant nextest tests. Put Rust unit tests in separate `<file>_tests.rs` files. Keep device claims unverified until the required device checks pass.

### 7. Implement Linux device and capability enumeration

Requires: 4.

Open questions:

- Which capability fields can the selected Wayland compositor or XRandR backend report, and how can its output identity be matched to colord?

TODO:

- Add the native output queries in [Platform implementations](../design/display_device.md#platform-implementations). Keep capability discovery usable when the profile service is absent. Use compositor or DRM colour state only where available.
- Verify missing protocols, unknown HDR or depth, output replacement and ambiguous identity with injected calls. Check reported fields on the selected desktop. Missing metadata must not be replaced with guessed display values.

### 10. Test FP16 conversion and neighbour search independently

TODO:

- Add independent PQ, primary-matrix and 80 cd/m² vectors, including negative components. Extend the existing grey test with per-level BT.2111 results and fixed difficult cases for the deliberate ±2-neighbour search.
- Check native D65 matrices with primary and white vectors. Keep ICC D50 adaptation, assumed monitor white and rounded EDID coordinates out of the scRGB reference calculation.
- Verify the stored bits, tie behaviour, search bounds and residual code errors under [QRhi output](../design/output_driver/testing.md#qrhi-output). Do not use the production inverse or a total failure count as the sole expected result.

### 11. Report QRhi capabilities and reject unknown FP16 scales

Requires: 2, 5, 6, 7, 9, 10.

TODO:

- Read physical device capabilities and mode through `DisplayDeviceControl` in `display.rs`. Remove duplicated native enumeration from `pattern_window.cpp`; keep its QRhi path-support checks. Combine the reports under the [feature matrix](../design/output_driver/overview.md#feature-matrix). Restrict the scale assumptions and comments in `output.rs` to the supported scRGB path.
- Verify that macOS 12-bit output fails before conversion with the documented scale reason. Keep supported 10-bit HDR10 available. Test unsupported swap chains without an SDR fallback, Windows HDR-off reporting, and unknown mode fields on other platforms. Linux compositor scale remains unverified until capture and device qualification.
- Test Windows SDR WCG separately from HDR. Generic Advanced Color enablement or an active GPU colour conversion must not alone set `hdr_on` to true. Unknown calibration or dithering state must not produce an exact-code claim.

### 12. Resolve and inspect QRhi metadata

Requires: 11.

TODO:

- Resolve automatic QRhi metadata from full-range PQ for every accepted encoding, including HLG scope input. Move native metadata packing behind an injectable bridge sink.
- Verify native field order, units, automatic and explicit values, disabled metadata and replacement. Use [QRhi metadata](../design/output_driver/qt.md#hdr10-static-metadata) as the contract. Keep the listed DXGI, Metal and display-effect questions open for device tests.

### 13. Acknowledge QRhi submission by frame ID

Requires: 11.

TODO:

- Replace success after `requestUpdate()` with a frame-ID callback after upload and successful `endFrame`. Report frame failures, closure and the queue-based timeout in [the QRhi design](../design/output_driver/qt.md#overview). Remove callback registrations on cancellation.
- With controlled render events, verify no early success, `beginFrame` and `endFrame` failures, timeout, close and two rapid replacements. Late work must not upload or complete a replacement request. Already submitted output can remain visible until replaced.

### 14. Implement asynchronous QrhiDriver

Requires: 2, 12, 13.

Open questions:

- Which existing executor integration can poll a local future and wake the Qt GUI thread? Check available libraries before adding a custom bridge.

TODO:

- Implement `OutputDriver` in `display.rs`. Run conversion in worker jobs with owned data; keep window and QRhi calls on the GUI thread. Resolve the future from bridge callbacks and wake the Rust task.
- Verify with controlled workers that Qt timers remain active during conversion and submission. Dropping the future must release its wait and callbacks without waiting for workers. Late results must not affect a new frame. Do not block or run a nested GUI event loop.

### 15. Make reference tables use output limits

Requires: 2, 10.

TODO:

- Supply selected-path limits to `pattern/catalogue.rs` and its reference table. Show interpretation warnings, FP16 precision marks and browser clipping marks from the relevant output contract.
- With mock path reports, verify that only the supported QRhi FP16 path has `*` marks. Browser nominal-range excursions must be marked as clipped at both depths. An unavailable path must not claim exact output.

### 16. Use the driver lifecycle in the Qt launcher

Requires: 14, 15.

TODO:

- Change `launcher.rs` and `qml/main.qml` to refresh native device state before the synchronous output query and open. Map Qt screens by native identity. Render at the returned size, pass metadata requests and retain frame data until completion or cancellation. Remove eager metadata calculation from `metadata_from_json`.
- Verify depth selection, mode reporting and warnings. Keep QRhi narrow PQ and HLG selectable. Replacement, display change and closure must drop the future before reusing or closing the driver. Controls must remain active, and late results must not change the current status.

### 17. Add the Qt measurement delay

Requires: 16.

TODO:

- Add the saved delay control, Qt timer and status transitions from [Measurement delay](../design/output_driver/overview.md#measurement-delay). Keep warnings in the ready status.
- With a controlled clock, verify acknowledgement plus delay, zero delay, negative-input rejection and cancellation. Failed presentation must start no timer. Verify the default and persistence across launcher restart.

### 18. Add the independent browser service

Requires: 2.

TODO:

- Add `nitpick-browser` with embedded client assets, `axum`, a dedicated Tokio thread and the separate Rust service handle. Supply URL, pairing-code and client-state events, plus shutdown. Use the [host boundary](../design/output_driver/overview.md#host-and-dependency-boundary).
- Verify start, bind failure, event delivery and shutdown on a Rust executor. Build by package and inspect its dependency graph: Qt, Tauri and `nitpick-app` must be absent. Loopback is the default; a LAN bind requires an explicit host setting.

### 19. Encode browser PNG samples and colour tags

Requires: 18.

Open questions:

- Does the selected `png` version expose the required colour and metadata chunks? Qualify its APIs; use `Writer::write_chunk` where needed.

TODO:

- Implement 16-bit RGB conversion and cICP output from [Code to PNG sample](../design/output_driver/browser.md#code-to-png-sample). Return clipping information with the encoded frame.
- Decode with an independent reader. Check every scalar code at both depths, inverse mapping in the nominal range, endpoint clipping, dimensions and channel order. Parse chunk placement and CRC independently. HLG must keep its HLG tag, with no competing colour tag.

### 20. Encode browser metadata from stored samples

Requires: 19.

TODO:

- Resolve automatic PQ light levels after PNG clipping and rounding. Reject automatic HLG light levels and accept explicit values. Write PNG `mDCV` and `cLLI` separately from the existing SEI helpers.
- Run [browser metadata and light-level checks](../design/output_driver/testing.md#browser-output). Verify chunk widths, order and units, disabled omission, the uniform 10-bit code-940 case, and values affected by sample rounding. Browser use of these chunks remains unverified.

### 21. Implement browser session authentication

Requires: 18.

TODO:

- Implement [Authentication](../design/output_driver/browser.md#authentication): reserve one session before authentication, issue pairing codes and session tokens, restrict failed attempts, and revoke tokens on disconnect. Protect frame requests with bearer tokens and no-store responses.
- With a mock transport and clock, verify leading zeros, configured fixed strings, wrong or out-of-order messages, authentication timeout, second-client rejection, unauthorized image requests, revocation and reconnect. Tokens must remain absent from URLs.

### 22. Cache browser state and validate surfaces

Requires: 2, 21.

TODO:

- Accept valid authenticated `hello` messages. Implement synchronous `displays`, `query` and `open` from cached state. Apply browser mode and capability rules, including user-agent limits, viewport size and pixel mapping.
- Verify no client before valid `hello`, disconnect removal, unknown precision and metadata effect, SDR preview and layout warnings. Resize or pixel-ratio change must invalidate the surface and emit a host event. A stale surface must fail presentation without a network wait.

### 23. Implement browser presentation and cancellation

Requires: 20, 22.

TODO:

- Implement `BrowserDriver::present` with owned encoding jobs, frame storage, `show`, matching `applied` replies and task wakeups. Run acknowledgement timeouts on the server runtime. Bound pending work and release obsolete frame data.
- Verify acknowledgement ID and dimensions, client errors, timeout from `show`, disconnect, resize, close and cancellation under [Image requests and presentation](../design/output_driver/browser.md#image-requests-and-presentation). Late worker results and replies must not complete a new frame. Service events must remain available during a mutable driver borrow.

### 24. Add browser authentication and viewport controls

Requires: 21, 22.

TODO:

- Add the static page's code entry, in-memory token, user-activated full-screen button and `hello` reporting. Report changes to media queries, viewport, full-screen state and pixel ratio.
- Test authentication failures and reconnect. Verify state updates at pixel ratios 1 and 2, fractional scale, zoom and full-screen changes. The page must work in a normal browser without Tauri.

### 25. Add browser image application and acknowledgement

Requires: 23, 24.

TODO:

- Fetch PNGs with the bearer header. Manage blob URLs, decode, image layout and acknowledgement as defined by [Image requests and presentation](../design/output_driver/browser.md#image-requests-and-presentation). Apply the specified CSS pixel mapping.
- Test natural and layout sizes, HTTP and decode errors, object-URL release, and replacement during fetch or decode. A slow old frame must not replace a new image or the black close page. Verify that `applied` follows decode, DOM update and the required animation callbacks.

### 26. Add the command-line session host

Requires: 15, 23.

TODO:

- Add `nitpick-cli` with server, pattern and format options. Print URL, pairing codes, client state, output limits and status. Select clients by `DisplayId`, query before open, and render at the returned size. Await the borrowed future on a local Rust executor.
- With a mock driver and service, verify startup, selection, unsupported requests, replacement and exit. Process console input and service events while presentation is pending. Close only after releasing the future; keep the service and current frame alive until replacement or exit.

### 27. Add command-line measurement timing

Requires: 26.

TODO:

- Add the delay option and executor timer using the shared [measurement-delay contract](../design/output_driver/overview.md#measurement-delay). Print readiness with the driver warnings.
- Run the controlled-clock host checks for default, zero and invalid delay, failed presentation, replacement, viewport change, disconnect and exit. A stale timer must not report readiness for a new frame.

### 28. Verify the browser session without a window system

Requires: 27.

TODO:

- Run a real server and CLI with an authenticated mock client. Render with the core, fetch and independently decode the PNG, send `applied`, check readiness, replace the frame and shut down.
- Verify timeouts and disconnects during presentation and delay. Build by package without Qt, Tauri or a window system; check the dependency graph. Document the implemented CLI commands and flags in the design usage section.

### 29. Connect browser output to the Qt launcher

Requires: 17, 25, 28.

TODO:

- Add browser driver selection, service events, pairing display and the explicit remote-display setting. Populate clients from the service and use the existing host lifecycle and delay.
- Verify local and remote browser sessions, URL reporting, warnings and reference marks. A disconnect during delay must cancel readiness. A viewport change must cancel old work, reopen and render at the new size without blocking Qt controls.

### 30. Add the optional local webview process

Requires: 25.

TODO:

- Add `nitpick-webview` as a separate Tauri process with URL and screen-position arguments. Apply the platform rules in [Pixel mapping](../design/output_driver/browser.md#pixel-mapping), including the Windows scale argument.
- Verify monitor selection, full-screen layout, authentication and process exit. Report the Linux SDR limit. Build and run the browser service and CLI without this package.

### 31. Launch the optional webview from Qt

Requires: 29, 30.

TODO:

- Add the local-webview selection and process lifecycle in [Usage](../design/output_driver/browser.md#usage). Derive the monitor position from Qt; keep screen geometry out of the server.
- Verify launch when no session exists, pairing, the post-authentication `hello` timeout, process failure and screen changes. An existing client must not be replaced by another session.

### 32. Verify the real page in browser engines

Requires: 25, 28. Test the optional shell after 30.

TODO:

- Run the page checks in [Browser integration](../design/output_driver/testing.md#browser-integration) on each supported engine. Record engine and OS versions. Include resize, zoom, full-screen changes and cancellation during fetch or decode.
- Verify authenticated fetch, layout, acknowledgement and resource release. Use screenshots for geometry only. Keep HDR pixel and metadata-effect claims unverified until qualified capture or physical measurement.

### 33. Add the shared test-host protocol

Requires: 17, 28.

TODO:

- Implement the JSON-lines requests and events in [Host boundary](../design/output_driver/testing.md#host-boundary). Use the real Qt adapter and CLI browser host, shared fixture IDs, frame IDs and surface generations. Keep capture outside `OutputDriver`.
- Drive both hosts from a test process. Verify query, open, show, close, error and readiness events, including replacement and malformed input. Run the shared host contract with both executors. An unavailable capture adapter must return a reason.

### 34. Add capture descriptors and result artifacts

Requires: 33.

TODO:

- Add the raw-plane capture interface and result writer from [Host boundary](../design/output_driver/testing.md#host-boundary). Save source and frame identity, colour descriptors, raw API metadata, run settings and unmodified samples. Keep previews separate.
- With synthetic captures, verify row pitch, channel order, plane layout, unknown fields and artifact round trips. Return `pass`, `fail` or `unavailable` with a reason. Bound artifact queues and reject invalid descriptors without inventing a colour space.

### 35. Reject stale captures and sample valid regions

Requires: 34.

TODO:

- Add the binary frame marker, optional heartbeat, bounded capture wait and region selection from [Timing and frame identity](../design/output_driver/testing.md#timing-and-frame-identity).
- Replay synthetic frame streams. Verify timestamps, current marker, three matching frames, deadline, resize and display-state changes. Test inset patch sampling separately from one-pixel geometry. Do not resize a capture to pass comparison.

### 36. Add comparison metrics and fixed limits

Requires: 34.

TODO:

- Implement component errors and the independent [reference metric](../design/output_driver/testing.md#reference-metric). Store per-case numeric bounds and the proposed perceptual target from [Pass criteria](../design/output_driver/testing.md#pass-criteria).
- Verify analytical cases, primary vectors and the BT.2124 worked example. Reject nonfinite values and invalid metric domains. Preserve signed values; require an explicit HLG viewing model. Check that FP16 search error is counted once and that a failed case cannot widen its limits.

### 37. Verify QRhi offscreen draw readback

Requires: 9, 10, 14, 34.

TODO:

- Add readback after the production upload, shader and pipeline draw to a supported offscreen target. Normalize only the documented row pitch and image origin.
- On D3D11, Metal and Vulkan, compare exact stored samples and geometry against CPU output. Include asymmetric corners, odd widths, channel ramps and alpha. A software device result covers arithmetic only, as defined by [QRhi readback](../design/output_driver/testing.md#qrhi-readback).

### 38. Verify QRhi swap-chain setup and failure handling

Requires: 12, 17, 33, 37.

TODO:

- Record native swap-chain format and colour space. Read back the actual swap-chain image after draw where supported. Keep an unavailable swap-chain readback distinct from an offscreen pass.
- On each backend, verify supported depths, geometry, metadata sink values, delayed submission, out-of-date surfaces, device loss, close during upload and rapid replacement. Success must name the submitted frame; an upload-texture dump cannot pass this check.

### 39. Add Windows FP16 capture

Requires: 34, 35.

TODO:

- Add the WGC window adapter in [Platform capture adapters](../design/output_driver/testing.md#platform-capture-adapters). Keep FP16 through storage and preserve content size, timestamps and raw metadata.
- Verify negative and greater-than-one samples, row layout, resize and window identity. Report permission or API failure with a reason. Record the submitted colour tag; do not infer the capture colour contract from FP16 storage.

### 40. Add macOS HDR capture

Requires: 34, 35.

Open questions:

- Which ScreenCaptureKit preset and high-precision format preserve the required signal on the selected macOS runner? Absolute luminance remains unverified until qualification.

TODO:

- Add the selected HDR stream adapter and retain pixel-buffer attachments, range and preset. Handle permission failure and any explicit YCbCr conversion as specified by [Platform capture adapters](../design/output_driver/testing.md#platform-capture-adapters).
- Verify buffer layout, timestamps and resize handling. Qualify on supported hardware before numeric HDR comparisons. Do not assign the Windows scRGB scale to generic EDR values or enable QRhi 12-bit output.

### 41. Add Linux capture with an explicit colour contract

Requires: 34, 35.

Open questions:

- Can the pinned compositor and portal backend provide high-precision samples and all required colour fields? Its capture stage and absolute scale remain unverified.

TODO:

- Add the ScreenCast portal/PipeWire adapter, or the required compositor adapter, from [Platform capture adapters](../design/output_driver/testing.md#platform-capture-adapters). Record negotiated samples and colour state.
- Verify layout, timestamps, resize and missing-field handling. Missing transfer, primaries, range or scale must make absolute HDR comparison unavailable. Keep X11 screenshots limited to geometry.

### 42. Add known ICC and MHC2 profile fixtures

TODO:

- Add complete display ICC fixtures for identity, matrix-only, LUT-only and combined calibration. Define matrix, curve and luminance expectations before serialization. Keep other display metadata equal across transform comparisons.
- Include ordinary ICC profiles without MHC2 for cross-platform assignment tests. Reserve MHC2 activation checks for Windows.
- Store both intended values and expected signed 15.16 values after serialization. State the rounding rule and test positive and negative half steps. Add a red/green exchange whose XYZ matrix is derived from its intended RGB action in the native sRGB/D65 basis.
- Give MHC2 minimum and peak display luminance, ICC `lumi` full-frame luminance and frame MaxCLL/MaxFALL distinct values in the manifests.
- Add the malformed-input and curve-boundary cases in [MHC2 profile adapter](../design/output_driver/testing.md#mhc2-profile-adapter). Verify fixture manifests, hashes and reproducible bytes. Keep profile extraction checks separate from later Windows installation checks.

### 43. Add and qualify the Little CMS profile adapter

Requires: 42.

Open questions:

- Which pinned `lcms2` binding and native Little CMS build pass every required fixture on all three operating systems? Native build settings and versions must be recorded separately.

TODO:

- Add the test-support adapter under [Colour library boundary](../design/output_driver/testing.md#colour-library-boundary). Use library tag access and return owned Rust data. Keep native handles, GUI dependencies and Windows APIs out of the model interface.
- Verify extracted values against manifests, including identity fields, signed values, bounds failures and unsupported LUT counts. Missing or unreadable tags must not silently become identity. Qualify the exact native build; resolve valid-profile failures before enabling the case.
- Test relocated tag data and internal offsets; do not assume a fixed file offset or contiguous matrix and LUT storage. Cover zero matrix offset, zero LUT count, two-entry identity curves and a nonzero ignored fourth matrix column. Compare decoded values to the serialized expectations from item 42.

### 44. Implement Windows ICC profile assignment

Requires: 5, 42.

TODO:

- Implement profile target discovery, preparation, assignment, readback and restoration in `nitpick-display`. Use native installation and association APIs with explicit scope and ordinary or Advanced Color role.
- Inject failure after each native write. Verify restoration, permission errors, no prior override, existing shared profiles and device replacement. On a selected test display, assign a known fixture and restore its original state. Verify OS assignment separately from MHC2 activation.

### 45. Implement macOS ICC profile assignment

Requires: 6, 42.

TODO:

- Implement the same trait operations through ColorSync device UUIDs and profile IDs. Preserve user and host scope. Restore either the previous custom profile or the absence of an override.
- Test rejected profiles, permission errors, partial failure and recovery with injected calls. Verify assignment and restoration on a selected Mac with ordinary ICC fixtures. A successful assignment must not claim MHC2 execution or measured HDR luminance.

### 46. Implement Linux ICC profile assignment

Requires: 7, 42.

TODO:

- Implement profile registration, device association, default selection and restoration through colord. Use the verified output-to-device mapping and preserve the original association state.
- Test a missing service, authorization failure, ambiguous mapping, shared profiles and partial failure. Verify readback and restoration on the selected desktop. Report compositor use of the profile separately; unavailable profile control must not break device enumeration.

### 47. Add display-state restoration and crash recovery

Requires: 4, 34.

TODO:

- Add ICC restoration and crash recovery through `DisplayDeviceControl`. Save the prepared change before mutation and record other display settings under [Usage and restoration](../design/output_driver/testing.md#usage-and-restoration). Remove only native resources created by the change. Keep automatic mode and vendor-control setters outside this work.
- Include legacy gamma ramps, external calibration loaders, GPU gamut clamps and dithering in the recorded setup. Establish a baseline without competing transforms. Recheck it after HDR, topology or resume events, because background tools can reapply calibration. Record unreadable state as unknown; do not assume that an unsupported NVAPI call cleared it.
- With a fake device backend, verify restoration after success, partial failure, timeout and cancellation. Simulate a crash and recover on the next run. Test no prior override, external state conflicts and repeated restoration. A restoration mismatch must fail the run.

### 48. Connect device operations to both hosts and tests

Requires: 16, 27, 33, 47. Native checks use the applicable profile backend from 44, 45, 46.

TODO:

- Add the shared device worker and test-host requests from [Host boundary](../design/output_driver/testing.md#host-boundary). Use the trait for profile changes in the Qt host, CLI and calibration runner. Keep CLI device control behind an optional feature.
- Verify responsive event loops, saved recovery records and serialization when a native write outlives a timeout or cancellation. Before profile assignment, cancel presentation and readiness. After readback, refresh device state, reopen as needed and submit a new frame.
- Test a local display mapping and reject a remote browser client as a local device identity. Verify stale callbacks and captures cannot qualify the new profile state. Build and run the browser-only CLI without the native device feature.

### 49. Verify conventional ICC transforms separately

Requires: 43.

TODO:

- Add the adapter's conventional ICC transform tests with explicit formats, intent, adaptation and black-point compensation settings. Use independent identity, transfer-curve and primary-conversion expectations.
- Use profiles with known white points and `chad` values to distinguish native XYZ from D50 PCS values. Verify that adaptation occurs only where requested. Keep any `vcgt` loading separate from ordinary ICC pixel conversion; do not apply calibration twice.
- Verify numeric bounds and record all options and library versions. Keep these results separate from MHC2 model and Windows calibration results. Account for the library's f32 stage precision where stages are used.

### 50. Add the explicit HDR display model

Requires: 36, 43.

TODO:

- Implement [Model order and domain](../design/output_driver/testing.md#model-order-and-domain) as portable test support. Require source and wire spaces and luminance scales. Keep conventional ICC adaptation separate from MHC2 evaluation.
- Apply the extracted MHC2 LUT directly after wire transfer encoding. Do not add an inverse ICC TRC or `vcgt` stage to a LUT that already contains that calibration. Keep NVAPI's SDR LUT construction and its 8-bit sample optimization out of this model.
- Verify identity, XYZ gain, asymmetric matrices, channel LUTs and an order-sensitive combined case against independent vectors. Check knots, interpolation, clipping and quantization; retain pre-quantization floats. Report these as ideal model results, not GPU behaviour.
- Use the RGB exchange fixture to detect a wrong matrix basis or transpose. Test sRGB and gamma 2.2 as distinct SDR transfers, including near-black values. Keep the runner's actual SDR transfer unverified until measured or established by its wire contract.

### 51. Qualify capture adapters and compare output

Requires: 32, 35, 36, 38, 47, 48, plus the applicable adapter from 39, 40, 41. Windows profile comparisons also require 43, 50.

TODO:

- Run [Capture reference and qualification](../design/output_driver/testing.md#capture-reference-and-qualification) per OS, backend and browser engine. Include black, HDR neutrals, wide-gamut colours, geometry and a separate tagged sRGB control. Save accepted frame timestamps and encoded browser PNGs.
- Compare QRhi against submitted values and ideal colour separately. On Windows, compare calibration profiles with equal display metadata and restore settings. Do not use MHC2 as a portable calibration test. Record capture conversion and colour scale; unknown contracts must not pass absolute HDR checks. Windows SDR-white behaviour and Linux scRGB scale need measured results.
- Record whether calibration changes capture. Leave the calibration boundary unverified until the IDD or physical comparison is available. Capture can change composition; the physical check must also run without capture.

### 52. Model IDD callback state without an installed driver

Requires: 50.

TODO:

- Add portable callback records and state replay from [Instrumented Windows IDD](../design/output_driver/testing.md#instrumented-windows-idd). Model enable flags, the scalar, affine matrix, LUT, transform generation and HDR metadata selection.
- Verify disabled stages, non-unit scalar, nonzero affine terms, replacement, reset and new/previous/default metadata. Reject previous metadata before any exists. Keep the active IDD affine column distinct from the ignored profile matrix column; avoid scaling HDR twice with SDR white.

### 53. Qualify the optional console IDD runner

Open questions:

- Which pinned console IDD revision supports the required surface, gamma and metadata callbacks? Build, signing and runtime behaviour remain unverified.

TODO:

- Audit and build the candidate in [Instrumented Windows IDD](../design/output_driver/testing.md#instrumented-windows-idd). Provision signing manually on a dedicated runner. Check runtime API availability, HDR modes and display state; the harness must not change boot security.
- Record the revision, build and qualification result. An advertised HDR EDID alone must not qualify the runner. Repeat qualification after a driver or OS update. Use this trial and the capture trial before estimating the remaining lab work.

### 54. Instrument IDD surfaces and transform generations

Requires: 34, 52, 53.

TODO:

- Record acquired surface bytes, colour metadata, committed wire mode, gamma callbacks and metadata selection. Copy surfaces before ownership expires. Capture the IDD's own transformed output before its encoder.
- Verify that each output uses one recorded transform generation, including callback changes during processing. Replay captured input and state against the portable model. A CPU recomputation must not substitute for the driver's transformed artifact.

### 55. Verify Windows profile loading through the IDD

Requires: 43, 44, 47, 48, 50, 54.

Open questions:

- Does the selected runner load each new profile without stale calibration? MHC2 activation and legacy gamma-ramp loader settings are separate; qualify the actual transition on this runner.

TODO:

- Assign the known profiles through `DisplayDeviceControl` on the qualified runner. Test HDR and SDR WCG separately, holding other display metadata and controls fixed. Record actual source and wire spaces.
- Check activation through the received transform generation and its effect. A successful profile association or a calibration-loader setting is not proof of activation. Do not use an NVAPI colour-conversion call as an MHC2 loading fallback. Test a missing or stale transform as a qualification failure.
- Compare effective matrix action, sampled curves, raw surfaces and driver output under [Instrumented Windows IDD](../design/output_driver/testing.md#instrumented-windows-idd). Allow recorded callback representation and resampling; reject unexplained transforms. Verify restoration and retain evidence. A pass applies only to that IDD path.

### 56. Add cross-platform execution gates

Requires: 28, 33, 36, 43, 48, 49, 50. Add device jobs after their qualification items pass.

TODO:

- Add the ordinary CI jobs and separate GPU, HDR, calibration and release jobs from [Run matrix](../design/output_driver/testing.md#run-matrix). Run core, driver, fixture and mock protocol checks on Windows, Linux and macOS without a virtual display.
- Run native device query and profile contract tests with injected calls on each target OS. Keep actual profile mutations in selected-device integration jobs. Verify routing and result aggregation with injected pass, fail and unavailable results. A required device losing qualification must fail; an optional unavailable device must not count as HDR coverage. Record conversion, encoding, acknowledgement, capture and settle times separately. Fail bounded-wait and queue-growth checks independently of pixel results.

### 57. Record physical output qualification

Requires: the relevant host and driver checks, 47, and qualified instruments. Use 51 only for comparisons that include OS capture.

TODO:

- Run the pending QRhi checks in [the measurement plan](test_patterns.md). Add release measurements for browser output, including a remote display, using [Pass criteria](../design/output_driver/testing.md#pass-criteria) and [Usage and restoration](../design/output_driver/testing.md#usage-and-restoration).
- Record wire samples and metadata where a cable exists, repeated probe readings, quantized references, uncertainty, display controls and delay. Check FP16 exceptions and above-peak tone mapping separately. A server capture cannot qualify a remote browser's light output.
- Record actual wire depth and dithering state separately. Check repeated wire samples for temporal changes; one frame cannot establish code preservation. Keep gamma-ramp, GPU-clamp and MHC2 effects isolated under the baseline from item 47.
- Store results at their measured boundary, verify restoration, and update only the claims supported by those results. Remove completed plan items without renumbering the remaining items.
