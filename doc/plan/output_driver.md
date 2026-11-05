# Plan: output driver corrections

These changes apply to existing code. The interfaces and behaviour are in [the output driver design](../design/output_driver/overview.md). Complete these changes before the device checks in [the measurement plan](test_patterns.md).

### 1. Restrict FP16 output to a known luminance scale

Open questions:
- Does the Linux compositor preserve the 80 cd/m² scRGB scale of the selected Vulkan colour space?

TODO:
- In `pattern_window.cpp`, report the available output paths and reject 12-bit output on macOS before frame conversion. Expose this result through `display.rs` and the launcher. Verify that the 12-bit option is disabled with the documented reason and that a forced request also fails. Keep supported 10-bit HDR10 output available.
- Restrict the 80 cd/m² assumptions in `output.rs` to the supported path. Update its comments and the reference-decoder descriptions. Verify that existing 10-bit packing and supported 12-bit conversion tests keep their results.

### 2. Separate accepted input from display interpretation

TODO:
- Add the common driver types in `nitpick-core::driver` and a QRhi adapter around `display.rs`. Report depth availability, code precision, accepted encoding, interpretation and automatic light-level support separately. Test rejection and warning rules with a mock driver.
- Update `launcher.rs` and `qml/main.qml` to use these reports. Keep QRhi `pq-narrow` and `hlg-narrow` selectable with interpretation warnings. Verify that `bt2111` HLG can still reach the QRhi bridge and that unsupported depths fail before rendering.
- Change `pattern/catalogue.rs` so FP16 `*` marks depend on the selected output path, rather than bit depth alone. Supply output limits to the reference table. Verify that the QRhi marks stay correct, a browser table has no QRhi FP16 marks, and levels outside the browser nominal range are marked as clipped.

### 3. Calculate metadata from the effective output signal

TODO:
- Change `metadata::light_level` to accept normalized PQ samples from the driver's selected conversion. Keep its maximum and frame-average calculation shared. Test a uniform 10-bit code-940 frame: full-range PQ gives about 4654.397 cd/m²; narrow-range PQ mapped to PNG peak gives 10000 cd/m².
- Replace the eager calculation in `launcher.rs::metadata_from_json` with `Hdr10Request`. Resolve automatic values in the driver after its range conversion. Verify that QRhi uses full-range PQ for every input encoding, including HLG scope input. Verify that explicit values bypass the calculation and disabled metadata causes no calculation.
- Add tests for the metadata request rules used by the browser path: automatic HLG values fail; explicit values are accepted; PQ sample clipping and 16-bit rounding are included in automatic values. Keep PNG chunk packing separate from the existing SEI byte helpers.

### 4. Acknowledge QRhi frame submission by frame ID

TODO:
- Replace immediate success after `requestUpdate()` in `pattern_window.cpp` with a completion callback for the submitted frame. Report success only after its upload and successful `endFrame`. Report `beginFrame` and `endFrame` failures, closure and submission timeout through `display.rs`.
- Keep the normal Qt event loop active. Use frame IDs for queued work and completion callbacks. Verify that a delayed render event gives no early success and that cancellation removes the callback without leaving a reference to a dropped request.
- Test two successive requests with a late result from the first. Only the current frame may be uploaded or complete its presentation. A frame already submitted before cancellation is allowed to remain visible until replaced.

### 5. Integrate asynchronous presentation and the measurement delay

Open questions:
- Which existing Rust executor or Qt integration can poll a local future and wake the Qt GUI thread? Check available libraries before adding a custom bridge.

TODO:
- Implement `PresentFuture` and the QRhi adapter. Poll it on the Qt GUI thread, move CPU conversion to worker jobs with owned data, and resolve it from the bridge callback. Do not block or start a nested event loop. Verify that Qt timers and controls remain active while conversion and submission are pending.
- Update `launcher.rs` and QML to retain each request's frame data until completion or cancellation. Drop a pending future before closing or reusing its driver. Verify new-frame requests, display changes, closure and failure; late worker or bridge results must not change the new request's status.
- Add the saved measurement-delay control and the statuses defined in [Measurement delay](../design/output_driver/overview.md#measurement-delay). Start its Qt timer only after the presentation future succeeds. Preserve driver warnings in the ready status.
- Test with a controlled clock: readiness follows acknowledgement plus the configured delay; zero adds no wait; invalid negative input is rejected; failed presentation starts no delay; cancellation prevents stale readiness. Verify that the setting survives a launcher restart.
- Run formatting, Clippy and the relevant nextest tests after these code changes. Add Rust tests in separate `_tests.rs` files.
