# Plan: output drivers

Implement [the output driver design](../design/output_driver/overview.md), [display device operations](../design/display_device.md) and [Output verification](../design/output_driver/testing.md). The design defines the contracts and pass criteria. This plan lists the remaining work in dependency order. Each item states its prerequisites and a completion check.

Only listed prerequisites block an item. Items without a `Requires` line have no prerequisites. Optional webview and IDD work does not block the core, browser or command-line host. Physical QRhi device checks remain in [the measurement plan](test_patterns.md); complete the relevant driver work before each check.

For code changes, run formatting, Clippy and the relevant nextest tests. Put Rust unit tests in separate `<file>_tests.rs` files. Keep device claims unverified until the required device checks pass.

### 16. Use the driver lifecycle in the Qt launcher

Requires: 15.

TODO:

- Build and run the macOS screen mapping on a Mac. On GNOME 50, map both screens with the Wayland and xcb Qt platforms; on xcb, check that the RandR read gives the Xwayland source. These run outside unit tests, which use injected device lists.
- In the running launcher on macOS and on GNOME 50 (Wayland and xcb), verify that Qt timers and controls remain active during the device read, conversion and submission. Change the selected display and add or remove a screen while a presentation is pending: the status must show the cancellation, and a late result must not change it. Windows passed; use the method in [Running launcher checks](../design/output_driver/qt.md#running-launcher-checks).
- On GNOME 50 on Wayland, `open` waits 0.3 to 0.5 s in its nested event loop. Add or remove a screen during this wait: the launcher must post the refresh again, and the status must show the cancellation. This did not occur on Windows, where `open` is shorter.

Open questions:

- Is a separate running check of a slow device read necessary? On Windows, the device read, `open` and render took less than 0.1 s together, so the check could not hold the read open. The `session.rs` unit tests use a held reader.

### 17. Add the Qt measurement delay

Requires: 16.

TODO:

- In the running launcher on macOS and on GNOME 50, verify that the Qt timer gives "Ready after delay" after the set time, and that Esc, a new frame and a display change during the delay cancel it. Verify that the delay value persists across a launcher restart in the OS configuration directory. Windows passed; see [Running launcher checks](../design/output_driver/qt.md#running-launcher-checks).

### 29. Connect browser output to the Qt launcher

Requires: 17, 25, 28.

TODO:

- Add browser driver selection, service events, pairing display and the explicit remote-display setting. Populate clients from the service and use the existing host lifecycle and delay.
- Verify local and remote browser sessions, URL reporting, warnings and reference marks. A disconnect during delay must cancel readiness. A viewport change must cancel old work, reopen and render at the new size without blocking Qt controls.

### 30. Add the optional local webview process

Requires: 25.

TODO:

- Run `nitpick-webview` on macOS, on Linux with X11 and with Wayland, and on Windows with two monitors at different scales. Verify `--list-monitors`, monitor selection by `--at`, the full-screen layout, authentication and exit. On Linux, record the WebKitGTK user agent and check the SDR output on an HDR display. Windows with one monitor passed; see [Browser output driver testing](../design/output_driver/browser.md#testing).

Open questions:

- In the shell, the page reports `fullscreen` only after the user selects "Full screen", although the window already covers the monitor. Until then, the panel covers part of the pattern and the host warns. Must the driver accept a full-screen shell window, for example when the viewport equals `screen`, and must the page hide the panel after authentication in the shell?

### 31. Launch the optional webview from Qt

Requires: 29, 30.

TODO:

- Add the local-webview selection and process lifecycle in [Usage](../design/output_driver/browser.md#usage). Derive the monitor position from Qt; keep screen geometry out of the server.
- On Windows, start `nitpick-webview` without a console window; it is a console program, so that its errors and `--list-monitors` output reach a terminal. Check that its window gets the keyboard focus when the launcher starts it; Windows can refuse a foreground change.
- Verify launch when no session exists, pairing, the post-authentication `hello` timeout, process failure and screen changes. An existing client must not be replaced by another session.

Open questions:

- Which Qt screen value gives the `--at` point? On Windows, check whether `QScreen::geometry()` keeps the native origin of each screen with mixed scales. On macOS and Linux, check that Qt and the shell use the same logical units, also with `QT_SCALE_FACTOR` or a GDK scale.
- Must the shell stop when the launcher ends unexpectedly? It runs until its window closes.

### 32. Verify the real page in browser engines

Requires: 25, 28. Test the optional shell after 30.

TODO:

- Run the page tests in [Browser output driver testing](../design/output_driver/browser.md#testing) with Safari on macOS and on iPadOS, Firefox on macOS, and Chromium and Firefox on Linux. Record engine and OS versions. Windows passed with Edge, Chrome and the Playwright Chromium, Firefox and WebKit builds; the Playwright WebKit build is not Safari. Safari on iPadOS needs a remote harness or a manual run; Playwright cannot drive it.
- Verify run-time browser zoom and pinch zoom outside Chromium, and full-screen geometry in a desktop browser. Playwright has no zoom call for Firefox and WebKit, and its full screen does not resize the OS window; use manual input or another harness. Use screenshots for geometry only.
- Find a working headed Firefox harness, or record why headless runs are sufficient. On Windows, headed Playwright Firefox 157 delays clicks and then its page process stops.
- Keep HDR pixel and metadata-effect claims unverified until qualified capture or physical measurement.

Open questions:

- Must `pixel_exact` accept a ratio within a small tolerance of an integer? Headed Chromium at 150 % OS scale reports emulated ratios as float32 values, for example 2.0000000596, and the driver gives `No`. **Unverified:** whether ordinary browser zoom levels give such values.
- Must `pixel_exact` accept a non-integer ratio when the layout size equals the image size? Edge at $r = 1.5$ showed one image pixel per display pixel in a screenshot. A screenshot cannot show resampling of HDR codes; a decision needs a qualified capture.

### 33. Add the shared test-host protocol

Requires: 17, 28.

The protocol, both hosts and their tests are in [Host boundary](../design/output_driver/testing.md#host-boundary) and [Shared host contract](../design/output_driver/testing.md#shared-host-contract). They pass on Windows.

TODO:

- Run the test-host tests of `nitpick-test-host`, `nitpick-cli` and `nitpick-app` on macOS and on Linux: the `offscreen` test without a display server, and the ignored screen test on a desktop (GNOME 50 with Wayland and with xcb). Record the results in the design.
- In the real Qt event loop, close the pattern window by Esc during a presentation and during the delay. Expect `error` with kind `disconnected` or `cancelled` with cause `disconnected`, then `surface` with `state: closed`. Only the contract checks this now, with a controlled close.

Open questions:

- The hosts read the fixtures from the source tree of the build (`nitpick_fixtures::fixture_dir`). Thus a packaged launcher with `--test-host` stops with exit status 2. Must the release build leave out the test-host mode, or include the fixtures?
- The host fills the surface around the fixture with nominal black, so automatic MaxCLL and MaxFALL differ from the fixture values (for example `seq-metadata-*`) unless the surface has the fixture size. Must a metadata check use a surface of the fixture size, or must the fixtures give the values of the filled frame?

### 61. Find why the D3D11 vblank thread sends no update requests

TODO:

- Without `QT_D3D_NO_VBLANK_THREAD`, `nitpick --test-host` on Windows 11 with Qt 6.12.0 got no `UpdateRequest` in the pattern window, and each frame timed out; see [Test host](../design/output_driver/qt.md#test-host). Find the cause, for example the adapter of the Qt vsync service, the screen (an HDR recorder with an 8 bpc wire), or the missing Qt Quick window. If the cause is in `pattern_window.cpp`, correct it there and remove the variable from the test host.
- Check whether the launcher depends on its Qt Quick window for the update requests: show a pattern with the QML window minimized, and with the QML window on another screen. A timeout of a frame is a failure.

### 62. Connect capture artifacts to the hosts

Requires: 34. Do this with the first capture adapter (39, 40 or 41); before it, both hosts give `unavailable` for every capture of a frame shown with the marker.

TODO:

- Add an artifact directory option to `nitpick --test-host` and `nitpick-cli --test-host`, and give `TestHost::artifacts` an `ArtifactWriter`. Select and document the queue size. Fill the `Environment` fields that each host can read: OS and build, GPU and driver, Qt version and QRhi backend, or browser and engine version from the client. Leave the EDID hash and the profile hash `null` until the device worker of item 48 gives them.
- Run a process test of each host with an artifact directory: `pass`, `saved`, the files and `read` of the artifact.

Open questions:

- At `quit`, the host waits for each queued write without a deadline, so a write that does not end blocks `stopped`. Must the wait have a deadline, and must the host then report `error` with kind `artifact` for each write that did not end?
- Raw API metadata is a JSON object plus named binary blobs. How must each adapter convert native metadata (Core Foundation dictionaries of pixel-buffer attachments, PipeWire SPA pods, WinRT properties) without loss? Record the rule in [Platform capture adapters](../design/output_driver/testing.md#platform-capture-adapters).

### 37. Verify QRhi offscreen draw readback

Requires: 9, 10, 34.

TODO:

- Add readback after the production upload, shader and pipeline draw to a supported offscreen target. Normalize only the documented row pitch and image origin.
- On D3D11, Metal and Vulkan, compare exact stored samples and geometry against CPU output. Include asymmetric corners, odd widths, channel ramps and alpha. A software device result covers arithmetic only, as defined by [QRhi readback](../design/output_driver/testing.md#qrhi-readback).

### 38. Verify QRhi swap-chain setup and failure handling

Requires: 17, 33, 37.

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
- The capture descriptor has no chroma siting field, and a sample read of a subsampled channel gives the block of the pixel. If a YCbCr format is selected, must the descriptor record chroma siting for the explicit conversion?

TODO:

- Add the selected HDR stream adapter and retain pixel-buffer attachments, range and preset. Handle permission failure and any explicit YCbCr conversion as specified by [Platform capture adapters](../design/output_driver/testing.md#platform-capture-adapters).
- Verify buffer layout, timestamps and resize handling. Qualify on supported hardware before numeric HDR comparisons. Do not assign the Windows scRGB scale to generic EDR values or claim absolute luminance for QRhi 12-bit output.

### 41. Add Linux capture with an explicit colour contract

Requires: 34, 35.

Open questions:

- Can the pinned compositor and portal backend provide high-precision samples and all required colour fields? Its capture stage and absolute scale remain unverified.

TODO:

- Add the ScreenCast portal/PipeWire adapter, or the required compositor adapter, from [Platform capture adapters](../design/output_driver/testing.md#platform-capture-adapters). Record negotiated samples and colour state.
- Verify layout, timestamps, resize and missing-field handling. Missing transfer, primaries, range or scale must make absolute HDR comparison unavailable. Keep X11 screenshots limited to geometry.

### 63. Qualify frame acceptance with a real capture adapter

Requires: the first adapter of 39, 40 or 41, and 62.

The frame marker, the gate, the heartbeat and region selection pass only with replayed synthetic frames; see [Timing and frame identity](../design/output_driver/testing.md#timing-and-frame-identity).

TODO:

- With the adapter, capture marked fixtures in both hosts. Check that the marker reads through the real capture conversion, also with YCbCr chroma subsampling, and that three matching frames arrive before the deadline. Record the frame time base of the adapter (for example whether WGC frame times are QPC values) and check it against `now`.
- Capture a static frame without and with `heartbeat_ms`. Record whether the platform delivers new frames for a static window and for each heartbeat presentation.
- Measure the time of the per-frame checks on the Qt GUI thread with a full-screen fixture. If Qt controls stop responding, move the checks to a worker and update the design.

Open questions:

- Matching frames must have equal stored samples (limit 0). Is this correct for real captures, for example with temporal dithering or capture noise? If not, which limit, and in which units?
- A frame of another size after the submission fails the capture at once. Can a transient resize after the submission occur, for example while a window becomes full screen? If yes, must such a frame be stale instead?
- The marker and each heartbeat presentation change the frame, so automatic `MaxFALL` (and `MaxCLL` of a fixture darker than the marker) changes; with automatic light levels each heartbeat can send new HDR metadata. Must a capture with metadata checks use explicit light levels, or must the heartbeat keep the metadata of the first presentation?

### 43. Add and qualify the Little CMS profile adapter

Open questions:

- Does a LUT offset of 0 with a nonzero LUT count select identity for that channel? Microsoft says that the 1DLUT elements "may be set to 0 (NULL)" for identity, but marks only the count as optional. No fixture has this case. Until a decision, the adapter returns `unsupported`.
- Must the adapter reject nonzero reserved fields, or a negative or reversed MHC2 luminance (minimum above peak)? The design gives no rule; no fixture has these cases. The adapter does not check them now.

TODO:

- Run `cargo nextest run -p nitpick-profile` on macOS and on Linux, in the test and release profiles, with the bundled Little CMS 2.19 build of `lcms2-sys` 4.0.7. Record the C compiler and the build profile. Windows passed in both profiles; see [MHC2 profile adapter](../design/output_driver/testing.md#mhc2-profile-adapter). A failure of a valid-profile case blocks that case until the adapter or the library is fixed.

### 44. Implement Windows ICC profile assignment

Requires: 5.

Open questions:

- Does SDR automatic colour management use `ordinary_p3_gamma22` in the Advanced Color role? Its D50 red colorant has $Z = -0.00105$, as in the Apple Display P3 profile. MHC2Gen states that SDR auto colour management fails for MHC profiles with negative colorants. Windows 11 build 26200 accepted the association and its readback; that does not show that the compositor uses the profile. **Unverified** for profiles without `MHC2`.

TODO:

- With each profile of the `mhc2` fixture set assigned, check that Windows accepts it as an MHC profile: the Advanced Color luminance reports (for example `DXGI_OUTPUT_DESC1`) must give the fixture's MHC2 minimum, MHC2 peak and `lumi` values. The values differ, so a swapped field shows. `IsColorProfileValid` does not read `MHC2`; it is not this check. Run it on a dedicated test display with `profile_roundtrip` or an equivalent runner: the design allows MHC2 fixtures only in the Windows calibration tests, and the example accepts only `ordinary` fixtures. It has not run; the only checked display belongs to a developer.

### 45. Qualify the macOS ICC profile assignment

Requires: 6.

The backend and its injected-call tests are in `nitpick_display::macos`; see [macOS profile assignment](../design/display_device.md#macos-profile-assignment). Clippy passed for `aarch64-apple-darwin` on Windows, and the injected-call tests passed on Windows. Nothing has run on a Mac.

TODO:

- On a selected Mac, run `cargo nextest run -p nitpick-display`. Then run `profile_roundtrip` with the `ordinary` fixtures in the current-user scope, with both host parts. Record the macOS version and the display. Check assignment, readback and restoration. Compare the output of `defaults read -g com.apple.ColorSync.Devices` and `defaults -currentHost read -g com.apple.ColorSync.Devices` before the assignment and after the restoration.
- Verify the undocumented storage that `scoped_custom_profiles` reads: the entry name `Device.mntr.<UUID>` with the UUID case, the `CustomProfiles` key, the profile ID as key, and the file path as value (not a URL). If it is different, correct the read and the design.
- Check that `kCFNull` removes the entry of the scope, so that a wider entry applies again. Set entries at two scopes and check the resolution order against `ColorSyncDeviceCopyDeviceInfo`.
- As root, check the any-user scope. Without root, record what an any-user call does: it returns false, or it returns true without a change.
- Record the registration scope of a built-in and an external display, and the targets that the backend lists.
- Check that `ColorSyncProfileVerify` accepts each `ordinary` fixture and rejects a damaged profile with a valid header.

Open questions:

- Must the backend use an identical profile file in a profile folder? The design says "Use an existing identical profile when possible". The macOS backend always installs a new file, because a planned resource holds one value and the record must name the file and the profile ID. A core change, for example a profile ID in the resource or the target, would allow reuse.
- Does ColorSync resolve each device entry separately, or the whole `com.apple.ColorSync.Devices` value of a scope, as `CFPreferences` resolves a key? In the second case, an entry of another device at a narrower scope hides the wider entries of this device. The check against the resolved value then makes the state unreadable, and the device has no usable target.

### 46. Qualify the Linux ICC profile assignment

Requires: 7.

The backend and its injected-call tests are in `nitpick_display::linux`; see [Linux profile assignment](../design/display_device.md#linux-profile-assignment). The tests cover a missing service, authorization failure, ambiguous mapping, shared profiles and partial failure. The native model comes from the colord 1.4.9 source. Clippy passed for `x86_64-unknown-linux-gnu` on Windows, and the injected-call tests passed on Windows. Nothing has run on Linux.

TODO:

- On the selected desktop (the Ubuntu 26.04 system of the Linux reports check, with colord 1.4.8), run `cargo nextest run -p nitpick-display`. Then run `profile_roundtrip` with the `ordinary` fixtures and the target `standard user`. Check assignment, readback and restoration. Compare the output of `colormgr get-devices-by-kind display` and `colormgr get-profiles` before the assignment and after the restoration.
- Check the native model on colord 1.4.8: the list order after `AddProfile` and `MakeProfileDefault`, the row with timestamp 0 after `RemoveProfile` (as root: `sqlite3 /var/lib/colord/mapping.db 'select * from mappings_v2'`), and that `DeleteProfile` removes the profile from the device. If it is different, correct the backend and the design.
- Run an assignment from an SSH session. Record whether colord returns `FailedToAuthenticate` (`PermissionDenied`), or polkit prompts and the call fails with `Timeout`.
- Stop colord. Check that the device list and the query work, that the device has no targets, and that no call starts colord.
- Restart colord after an assignment. Check that restoration removes the file and gives the original state.
- Report separately whether the compositor applies the assigned profile: GNOME, and KWin if available. For example, assign a profile with a visible tone change and capture or measure a pattern.

### 60. Decide the open colord assignment questions

Requires: 46.

Open questions:

- Must restoration remove the mapping database row of the change? colord has no call for it. The row has timestamp 0 and colord ignores it, but it remains in `/var/lib/colord/mapping.db`.
- Must the backend handle the colord defect in which a failed `CreateProfileWithFd` leaves a profile without a D-Bus object (see [Linux profile assignment](../design/display_device.md#linux-profile-assignment))? Restoration then fails until colord restarts. To treat an object that does not answer as absent would also hide other failures.
- Must the target scope be all users? colord stores the relation by device ID, so the session of another user with the same device ID can get it.
- Must the backend read the colord device property `Enabled`? A disabled device keeps its profile list. Whether a compositor then applies the default profile is unknown.
- Must colord writes have a longer timeout than the 2 s read timeout? A polkit password prompt takes longer, so the write fails with `Timeout` and can take effect later.

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

### 58. Decide the remaining conventional ICC transform cases

Requires: 43.

Open questions:

- Is a check of black-point compensation with a nonzero black point necessary? No fixture curve has one, so the setting has no effect on the [transform cases](../design/output_driver/testing.md#conventional-icc-transforms). Little CMS finds a black point through a 16-bit transform, so an independent expected value needs a stated tolerance.
- Are out-of-gamut values necessary? The generator does not accept a negative linear value at a gamma 2.2 profile. The Little CMS result for that input is not pinned.
- Must tests pin the Little CMS identity rules (0.002 for the absolute-intent conversion matrix, $10^{-5}$ for joined matrices)? No current case depends on them; a case with almost equal profiles or absolute intent to the PCS endpoint does.

### 59. Qualify the remaining Windows profile assignment cases

Requires: 44.

The current-user scope passed in both roles with HDR on, on one display without prior associations; see [Windows profile assignment](../design/display_device.md#windows-profile-assignment).

TODO:

- With `profile_roundtrip` and the `ordinary` fixtures, check the ordinary role with HDR off and in SDR WCG, and a display with existing associations in both roles. Check that the readback and restoration keep the user's associations and default.
- As an administrator, check the system-wide scope. Confirm that `ICMProfileAC` in the monitor driver key holds the system-wide Advanced Color list; until then that list source is unverified.
- With a system-wide list that is not empty, turn on the per-user switch through a current-user assignment. Check whether Windows copies the system-wide list into the current-user list, as the legacy `AssociateColorProfileWithDevice` reference states. A copy makes restoration fail with `StateConflict`; handle it if it occurs.
- Check the legacy device name lookup with two monitors on one GDI source (clone mode).

Open questions:

- Must restoration remove the current-user registry key `...\ICM\ProfileAssociations\Display\{class}\<n>` when the change created it? After restoration it holds empty lists and the per-user switch off, and the Windows functions report the original state. The real-device check removed it by hand to get a byte-equal registry.
- As a standard user, system-wide association calls returned success without a change, and the assignment failed at readback with `StateConflict`. Must the backend report `PermissionDenied` instead, for example by checking the process token before a system-wide write?
- `ColorProfileGetDisplayList` returned no profile while associations existed, so the backend reads undocumented registry values. Is there a documented read of both lists, or a Windows build where the function works?

### 50. Add the explicit HDR display model

Requires: 36, 43.

TODO:

- Implement [Model order and domain](../design/output_driver/testing.md#model-order-and-domain) as portable test support. Require source and wire spaces and luminance scales. Keep conventional ICC adaptation separate from MHC2 evaluation.
- Apply the extracted MHC2 LUT directly after wire transfer encoding. Do not add an inverse ICC TRC or `vcgt` stage to a LUT that already contains that calibration. Keep NVAPI's SDR LUT construction and its 8-bit sample optimization out of this model.
- Verify identity, XYZ gain, asymmetric matrices, channel LUTs and an order-sensitive combined case against independent vectors. Check knots, interpolation, clipping and quantization; retain pre-quantization floats. Report these as ideal model results, not GPU behaviour.
- Use the RGB exchange fixture to detect a wrong matrix basis or transpose. Test sRGB and gamma 2.2 as distinct SDR transfers, including near-black values. Keep the runner's actual SDR transfer unverified until measured or established by its wire contract.

### 51. Qualify capture adapters and compare output

Requires: 32, 35, 36, 38, 47, 48, 62, 63, plus the applicable adapter from 39, 40, 41. Windows profile comparisons also require 43, 50.

TODO:

- Run [Capture reference and qualification](../design/output_driver/testing.md#capture-reference-and-qualification) per OS, backend and browser engine. Include black, HDR neutrals, wide-gamut colours, geometry and a separate tagged sRGB control. Save accepted frame timestamps and encoded browser PNGs.
- Compare QRhi against submitted values and ideal colour separately. On Windows, compare calibration profiles with equal display metadata and restore settings. Do not use MHC2 as a portable calibration test. Record capture conversion and colour scale; unknown contracts must not pass absolute HDR checks. Windows SDR-white behaviour and Linux scRGB scale need measured results.
- Record whether calibration changes capture. Leave the calibration boundary unverified until the IDD or physical comparison is available. Capture can change composition; the physical check must also run without capture.

### 64. Decide the comparison limits outside the binary16 scRGB capture

Requires: the applicable adapter of 39, 40 or 41 for the first TODO, and 51 for the second.

The metric, the check and the per-case bounds are in `nitpick-compare` and the output fixtures; see [Reference metric](../design/output_driver/testing.md#reference-metric) and [Pass criteria](../design/output_driver/testing.md#pass-criteria). The fixtures have bounds only for a binary16 scRGB capture of the ideal values.

TODO:

- With each capture adapter (39, 40, 41), derive and freeze the bounds of its capture storage and scale in the generator, for example 10-bit PQ, P010 or 8-bit samples. Until then, a comparison of such a capture has no limits and must be `unavailable`.
- At capture qualification (51), check the two assumptions of the bounds: the compositor and the capture keep binary16 swap-chain values unchanged, and the browser keeps more precision than binary16 from the PNG decode to the capture. If a check fails, change the bound in the generator after review, not in a test.

Open questions:

- Which HLG display (peak, black level, system gamma) does a browser HLG comparison use? `HlgDisplay` has no default. BT.2124 Annex 2 uses 1000 cd/m², $\gamma = 1.2$ and black 0, but a browser does not state its HLG rendering.
- With bounds from quantization, the perceptual check adds a failure only for a sample outside the bound box. Is the perceptual target then needed as a separate criterion of a capture check, or only for looser bounds (model and light comparisons)?
- Next to black, 19 entries of 12-bit fixtures have no quantization $\Delta E_{ITP}$, because the FP16 search range reaches below 0 and the metric has no value there. Only the component check applies to them. Is this sufficient?

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

- Add the ordinary CI jobs and separate GPU, HDR, calibration and release jobs from [Run matrix](../design/output_driver/testing.md#run-matrix). Run core, driver, fixture and mock protocol checks on Windows, Linux and macOS without a virtual display. Include the `nitpick-cli` process and dependency-graph tests on Linux and macOS without a display server or a window session; they passed only on Windows in a desktop session. Run the `nitpick-profile` adapter tests on each OS; the job needs a C compiler and must not set `LCMS2_LIB_DIR`. Build and unit-test `nitpick-webview` in a separate job on each OS; on Linux it needs the WebKitGTK and GTK development packages, and it has been built only on Windows.
- Run native device query and profile contract tests with injected calls on each target OS. Keep actual profile mutations in selected-device integration jobs. Verify routing and result aggregation with injected pass, fail and unavailable results. A required device losing qualification must fail; an optional unavailable device must not count as HDR coverage. Record conversion, encoding, acknowledgement, capture and settle times separately. Fail bounded-wait and queue-growth checks independently of pixel results.

### 57. Record physical output qualification

Requires: the relevant host and driver checks, 47, and qualified instruments. Use 51 only for comparisons that include OS capture.

TODO:

- Run the pending QRhi checks in [the measurement plan](test_patterns.md). Add release measurements for browser output, including a remote display, using [Pass criteria](../design/output_driver/testing.md#pass-criteria) and [Usage and restoration](../design/output_driver/testing.md#usage-and-restoration).
- Record wire samples and metadata where a cable exists, repeated probe readings, quantized references, uncertainty, display controls and delay. Check FP16 exceptions and above-peak tone mapping separately. A server capture cannot qualify a remote browser's light output.
- Record actual wire depth and dithering state separately. Check repeated wire samples for temporal changes; one frame cannot establish code preservation. Keep gamma-ramp, GPU-clamp and MHC2 effects isolated under the baseline from item 47.
- Store results at their measured boundary, verify restoration, and update only the claims supported by those results. Remove completed plan items without renumbering the remaining items.
