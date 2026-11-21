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

### 68. Verify the timer render trigger with HDR on and with two screens

The pattern window renders from its own timer, not from `requestUpdate`; see [Render trigger](../design/output_driver/qt.md#render-trigger). It passed on Windows with HDR off only.

TODO:

- With the consent of the machine owner, turn on HDR on the Windows test screen. Run the ignored tests `qrhi_swap_chain`, `wgc_capture` and `test_host` of `nitpick-app` (Vulkan with `VULKAN_SDK`). Expect all to pass without `QT_D3D_NO_VBLANK_THREAD`. Record the results in [Output verification](../design/output_driver/testing.md).
- Measure `IDXGIOutput::WaitForVBlank` with HDR on, and on an output that is not the recorder. Record whether it blocks for one refresh interval. This finds whether the recorder, the driver or the HDR-off state causes the immediate return.
- With two screens on Windows, show a pattern in the launcher with the Qt Quick window on the other screen. A timeout of a frame is a failure.
- On macOS and on GNOME 50 (Wayland and xcb), the test-host screen test of item 33 and the swap-chain test of item 38 run the timer path. A timeout of a frame there is a failure of this item.

Open questions:

- Must the defect be reported to Qt? Qt 6.12.0 `QDxgiVSyncService` gives no `UpdateRequest` and no timer fallback when `WaitForVBlank` returns within 1 ms.

### 62. Connect capture artifacts to the hosts

Both hosts take `--artifacts <directory>`, with a queue of 4 and the environment of [Capture interface and artifacts](../design/output_driver/testing.md#capture-interface-and-artifacts). The QRhi host passed the process test with an artifact directory on Windows (`pass`, `saved`, the files and `read`). The browser host has no capture adapter; its artifact test is in item 66. The macOS and Linux environment reads are in item 69. The EDID hash and the profile hash stay `null` until the device worker of item 48 gives them.

Open questions:

- At `quit`, the host waits for each queued write without a deadline, so a write that does not end blocks `stopped`. Must the wait have a deadline, and must the host then report `error` with kind `artifact` for each write that did not end?
- Raw API metadata is a JSON object plus named binary blobs. How must each adapter convert native metadata (Core Foundation dictionaries of pixel-buffer attachments, PipeWire SPA pods) without loss? Record the rule in [Platform capture adapters](../design/output_driver/testing.md#platform-capture-adapters). The WGC adapter has no such structure: it stores the typed WinRT and D3D11 values that it reads as JSON numbers and strings.

### 37. Verify QRhi offscreen draw readback

Requires: 9, 10, 34.

The offscreen readback and its test are in [QRhi readback](../design/output_driver/testing.md#qrhi-readback). D3D11 (GPU and WARP) and Vulkan passed on Windows.

TODO:

- On a Mac, run `cargo nextest run -p nitpick-app --test qrhi_readback --run-ignored all`. The Metal branch of `cpp/readback.cpp` and the moved `cpp/pattern_renderer.cpp` have not been built on macOS. Expect `metal_draw_matches_cpu_output` to pass; record the macOS version and the GPU in the design.
- On the Linux desktop (GNOME 50, Wayland and xcb), run the same command. Expect `vulkan_draw_matches_cpu_output` to pass. If Mesa lavapipe is installed, `vulkan_software_draw_matches_cpu_output` must pass; otherwise it fails because no CPU device exists, and the result is unavailable. Record the driver versions in the design. The test makes the QGuiApplication on a test thread, not on the main thread; record whether Qt accepts this on each platform.

Open questions:

- Must ordinary CI on Linux and macOS run a software readback, as Windows does with WARP? macOS has no software Metal device. On Linux the test needs lavapipe and a Qt platform that gives a Vulkan instance. **Unverified:** whether the Qt `offscreen` platform gives one.
- Must the Windows build find the Vulkan headers by default, so that the Vulkan backend is always tested on Windows? Now it needs `VULKAN_SDK`; without it the Windows Vulkan tests fail with the reason. The Windows window does not use Vulkan.

### 65. Check the pattern window initialization after a surface loss

TODO:

- `PatternWindow::init` runs again at an expose after `SurfaceAboutToBeDestroyed`. It replaced the `QRhi` before it destroyed the swap chain, the render pass descriptor and the pattern resources of the earlier `QRhi`; now it destroys them first. No test reaches this path. On each OS, hide and show a pattern window, or use another action that destroys its native surface. Check that the window initializes again without a crash or a Qt warning about leaked resources, and that the next frame gets its acknowledgement.

### 38. Verify QRhi swap-chain setup and failure handling

Requires: 17, 33, 37.

The native swap-chain record, the swap-chain readback and the failure checks with injected `QRhi` results are in [Swap-chain checks](../design/output_driver/testing.md#swap-chain-checks). D3D11 and Vulkan passed on Windows.

TODO:

- On a Mac with an EDR display, run `cargo nextest run -p nitpick-app --test qrhi_swap_chain --run-ignored only`. The Metal record (`metalLayerFormat` in `cpp/macos.mm`) and the Metal branches of `cpp/pattern_window.cpp` have not been built. Expect `metal_swap_chain` to pass. Check that `CGColorSpaceCopyName` gives the names of the record table in [Output report](../design/output_driver/qt.md#output-report); correct the table or the test if not. Record the macOS version and the GPU in the design.
- On the Linux desktop (GNOME 50, Wayland and xcb) with an HDR display, run the same command. Expect `vulkan_swap_chain` to pass at 10 bits. Record whether Qt accepts the `QGuiApplication` on a test thread, and whether the surface supports the swap-chain readback.
- With HDR off on Windows, open each depth (for example with the test host) and check the fallback record: `DXGI_FORMAT_R8G8B8A8_UNORM` with `DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709`, and the fallback warning in the status. The swap-chain test requires HDR on, so it does not cover this path. Do not change the display settings of a developer machine without its owner.

Open questions:

- GNOME 50 on Wayland gives no extended-linear sRGB colour space, so the 12-bit swap chain falls back there and the 12-bit part of `vulkan_swap_chain` fails at the record check. Must the test expect the fallback on such a compositor, or skip 12 bits with the reason?
- Must the window create a new `QRhi` after a device loss? Qt 6.12 gives device loss for each later `beginFrame`, so each frame fails with `Platform` until the host opens the window again. Neither host reopens by itself.
- Is a check with a real device loss or a real out-of-date swap chain necessary? The checks inject the `QRhi` results. A D3D11 device removal needs a TDR, which resets the GPU for all processes. A real out-of-date swap chain needs a resize of the full-screen window or a mode change.

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

### 66. Add Windows capture to the browser host

Open questions:

- Which window does `nitpick-cli --test-host` capture? The WGC adapter takes an `HWND`, and the browser host knows only the client state. The optional webview runs in the `nitpick-webview` process; an external browser needs a lookup by process or title. A remote browser cannot be captured on the server.
- WGC captures the whole top-level window. Must the adapter accept a browser window only in full screen, or give a content rectangle without the browser chrome?
- The artifact `renderer` of the browser host is the user agent of `hello`. A browser with a reduced user agent (Chromium) gives only its major version. Must the page also send the full version, for example from `navigator.userAgentData.getHighEntropyValues(["fullVersionList"])` where it exists?
- The artifact `gpu` of the browser host is `null`: the page does not report its GPU, and the command-line host has no GPU dependency. Must the page report it (for example WebGL `UNMASKED_RENDERER_WEBGL`), or must the host read the local GPU?

TODO:

- After the decision, give the browser host a WGC adapter for a local browser window. Verify the window identity and the content rectangle with the page in full screen and in a normal window.
- Run a process test of `nitpick-cli --test-host --artifacts <directory>` with a captured local browser: `pass`, `saved`, the files and `read` of the artifact, with `os` and `renderer` in the environment. The current process test checks only that `capture` is `unavailable` and that the directory stays empty.

### 69. Check the artifact environment on macOS and Linux

Requires: 40 for macOS, 41 for Linux.

The QRhi host reads the environment of an artifact from the open pattern window; see [Capture interface and artifacts](../design/output_driver/testing.md#capture-interface-and-artifacts). Only D3D11 on Windows has run. The Vulkan read has been compiled on Windows with `VULKAN_SDK`; the Metal branch has not been built.

TODO:

- With the capture adapter of each OS, run the ignored screen test of `nitpick-app` (`tests/test_host.rs`) with its artifact directory. `check_artifact` asserts the Windows `renderer` (`QRhi D3D11`) and a driver version in `gpu`; change these checks for Metal (no driver version) and Vulkan. Record the `os`, `gpu` and `renderer` values in the design.
- Check the `os_info` value: on macOS it runs `sw_vers`, on Linux it reads `/etc/os-release` or runs `lsb_release`. Both run once at host start, before the event loop.

Open questions:

- The Vulkan `driverVersion` has a vendor-specific bit layout, so the record gives it raw. Must the host read `VkPhysicalDeviceDriverProperties` (driver name and driver info, for example the Mesa version) instead? That needs Vulkan 1.2 or `VK_KHR_driver_properties`, and an instance of Vulkan 1.1 or `VK_KHR_get_physical_device_properties2`. `sharedVulkanInstance` sets no API version; **unverified:** which version Qt then requests.

### 67. Check the WGC failure paths and a capture with HDR off

TODO:

- A capture policy or a privacy setting that blocks screen capture must give `unavailable` with the reason. A window of the own process needed no permission on Windows 11 build 26200, so no failure path except a missing window has run. Find a Windows setting or policy that blocks WGC for an unpackaged process and check the reason. Use a dedicated test system.
- With HDR off, capture a 12-bit pattern window (fallback swap chain) and record the captured samples against the submitted words. `tests/wgc_capture.rs` requires HDR on; extend it or add a separate check. Do not change the display settings of a developer machine without its owner.

### 63. Qualify frame acceptance with a real capture adapter

The frame marker, the gate, the heartbeat and region selection are in [Timing and frame identity](../design/output_driver/testing.md#timing-and-frame-identity). With a real adapter, only the QRhi host on Windows was checked (the ignored process test of `nitpick-app`, WGC adapter): the marker read through the WGC FP16 capture of the HDR10 and the FP16 swap chain, and three matching frames arrived before a 10 s deadline with `heartbeat_ms` 100. Without a heartbeat, a static window gave one frame in 2 s. The WGC frame time is a QPC time; see [Windows Graphics Capture adapter](../design/output_driver/testing.md#windows-graphics-capture-adapter).

TODO:

- Capture marked fixtures in the browser host (after item 66) and with the macOS and Linux adapters. Check that the marker reads through each capture conversion, also with YCbCr chroma subsampling if an adapter gives it, and that three matching frames arrive before the deadline. Record the frame time base of each adapter and check it against `now`.
- Record whether each heartbeat presentation gives a new frame: count the received frames per heartbeat (WGC, ScreenCaptureKit, PipeWire). `captured` does not report the count of an accepted capture; use the artifact (item 62) or a log.
- Measure the time of the per-frame checks on the Qt GUI thread with a full-screen fixture. If Qt controls stop responding, move the checks to a worker and update the design.

Open questions:

- Matching frames must have equal stored samples (limit 0). Is this correct for real captures, for example with temporal dithering or capture noise? If not, which limit, and in which units? WGC FP16 frames on one Windows system matched with limit 0.
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

### 70. Decide the restoration and baseline rules

The recovery store, the profile run and the display setup check are in [Restoration and recovery](../design/display_device.md#restoration-and-recovery) and [Display setup](../design/display_device.md#display-setup). They use the conservative rules below until a decision.

Open questions:

- Must an `Unknown` setup finding fail a run? No backend reads the GPU gamut clamp or dithering, so no run has a clean baseline now. Until a decision, a `Competing` finding fails the run, and each `Unknown` finding stays in the result; the run must not claim a clean baseline.
- An OS calibration loader that is on is `Competing`. On the checked Windows system it was on. Is it acceptable when the ramp is identity and no assigned profile has `vcgt`?
- Dithering that is on is `Competing`. Some drivers dither by default. Must it fail light measurements, or only wire-code checks?
- After a timed-out assignment, the run keeps the record until the next `recover`. How long can a native write take effect after its timeout, for example a colord write behind a polkit prompt? Must the worker wait a stated time and verify again in the same run?
- A late write after a partial restoration (for example the selection after the association removal) gives a state that no prefix of the assignment steps gives. Restoration then gives `StateConflict` and keeps the record. Must restoration accept such a state?
- The store holds one record per device, so one run cannot change two targets of a device, for example both Windows roles. Is such a change necessary?
- On Windows the store does not sync the directory after the rename. Does the record survive a power loss right after `save`?

### 71. Read the GPU vendor controls on Windows

TODO:

- Read the GPU gamut clamp and dithering on NVIDIA through NVAPI: `NvAPI_GPU_GetColorSpaceConversion` (interface ID `0x8159E87A`) and `NvAPI_GPU_GetDitherControl` (interface ID `0x932AC8FB`, structure version `0x10018`). These IDs are not in the public NVAPI headers. Map the GDI display to the NVAPI display ID. A failure or an unsupported call gives unknown, not off. Verify with injected calls, then on an NVIDIA system without a change of the settings; compare with the NVIDIA control panel.
- Find the read calls of AMD ADL and Intel IGCL, or keep the fields unknown with the reason.

Open questions:

- Is a binding to undocumented NVAPI interface IDs acceptable? A driver update can change them.

### 72. Read the display setup on macOS and Linux

TODO:

- macOS: read the gamma table with `CGGetDisplayTransferByTable` and define its identity forms. Record whether ColorSync loads the `vcgt` of the assigned profile; that is the OS calibration loader. Keep the GPU gamut clamp and dithering unknown unless macOS reports them.
- Linux: read the KMS properties `GAMMA_LUT`, `DEGAMMA_LUT` and `CTM` of the CRTC of the connector; a property without a blob is identity. Record which process sets them (the compositor or a colour daemon that loads `vcgt`). KMS does not report dithering; keep it unknown.
- Verify with injected native calls. Then run the `devices` example on the selected Mac and Linux desktop and record the values in the design.

### 73. Check the Windows display setup read on a runner

TODO:

- On the Windows development system, `GetDeviceGammaRamp` failed for the only active display (an HDMI recorder, HDR off; its device key was the `target:` fallback, so the monitor device path was empty). Find the cause. On the runner, record the ramp of each display with and without a loaded calibration, and which identity form the driver gives.
- On a dedicated test system, set a ramp with a separate tool and check that `DisplaySetup::changes` reports `gamma_ramp`. With HDR on and in SDR WCG, record whether the GDI ramp changes the output, for example with a WGC capture. Do not change the display settings of a developer machine.

### 48. Connect device operations to both hosts and tests

Requires: 16, 27, 33. Native checks use the applicable profile backend from 44, 45, 46.

TODO:

- Add the shared device worker and test-host requests from [Host boundary](../design/output_driver/testing.md#host-boundary). Use the trait for profile changes in the Qt host, CLI and calibration runner. Keep CLI device control behind an optional feature.
- Open a `RecoveryStore` in a per-user state directory of each host and call `recover` before the first device request; report each kept record. Make each profile change through `ProfileRun`. A cancellation during `set_icc_profile` waits for the call on the worker and then restores; no other change of that device starts before. Use `nitpick_display::fake` (feature `fake`) in the host tests.
- Read the display setup before a device run and report its findings. Read it again after HDR, topology and resume events and before a pass; a change fails the run. Put the setup in the artifact environment.
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

### 51. Qualify capture adapters and compare output

Requires: 32, 35, 36, 38, 48, 63, plus the applicable adapter: 40 or 41 (the Windows adapter exists). Windows profile comparisons also require 43, 50.

TODO:

- Run [Capture reference and qualification](../design/output_driver/testing.md#capture-reference-and-qualification) per OS, backend and browser engine. Include black, HDR neutrals, wide-gamut colours, geometry and a separate tagged sRGB control. Save accepted frame timestamps and encoded browser PNGs.
- Compare QRhi against submitted values and ideal colour separately. On Windows, compare calibration profiles with equal display metadata and restore settings. Do not use MHC2 as a portable calibration test. Record capture conversion and colour scale; unknown contracts must not pass absolute HDR checks. Windows SDR-white behaviour and Linux scRGB scale need measured results.
- Record whether calibration changes capture. Leave the calibration boundary unverified until the IDD or physical comparison is available. Capture can change composition; the physical check must also run without capture.

### 64. Decide the comparison limits outside the binary16 scRGB capture

Requires: the applicable adapter of 40 or 41 for the first TODO, and 51 for the second.

The metric, the check and the per-case bounds are in `nitpick-compare` and the output fixtures; see [Reference metric](../design/output_driver/testing.md#reference-metric) and [Pass criteria](../design/output_driver/testing.md#pass-criteria). The fixtures have bounds only for a binary16 scRGB capture of the ideal values.

TODO:

- With the macOS and Linux capture adapters (40, 41), derive and freeze the bounds of their capture storage and scale in the generator, for example 10-bit PQ, P010 or 8-bit samples. Until then, a comparison of such a capture has no limits and must be `unavailable`. The WGC adapter stores binary16 samples, as the current bounds assume.
- At capture qualification (51), check the two assumptions of the bounds: the compositor and the capture keep binary16 swap-chain values unchanged, and the browser keeps more precision than binary16 from the PNG decode to the capture. If a check fails, change the bound in the generator after review, not in a test.

Open questions:

- Which HLG display (peak, black level, system gamma) does a browser HLG comparison use? `HlgDisplay` has no default. BT.2124 Annex 2 uses 1000 cd/m², $\gamma = 1.2$ and black 0, but a browser does not state its HLG rendering.
- With bounds from quantization, the perceptual check adds a failure only for a sample outside the bound box. Is the perceptual target then needed as a separate criterion of a capture check, or only for looser bounds (model and light comparisons)?
- Next to black, 19 entries of 12-bit fixtures have no quantization $\Delta E_{ITP}$, because the FP16 search range reaches below 0 and the metric has no value there. Only the component check applies to them. Is this sufficient?
- A WGC capture has no colour fields: WGC gives none, and the descriptor must not take scRGB from FP16 storage. Thus the scRGB expectation and its bounds apply to it only with a colour contract from qualification (51). Where does a comparison get that contract, for example a qualified-contract record per runner and display state, kept outside the capture descriptor?

### 53. Qualify the optional console IDD runner

Open questions:

- Which pinned console IDD revision supports the required surface, gamma and metadata callbacks? Build, signing and runtime behaviour remain unverified.

TODO:

- Audit and build the candidate in [Instrumented Windows IDD](../design/output_driver/testing.md#instrumented-windows-idd). Provision signing manually on a dedicated runner. Check runtime API availability, HDR modes and display state; the harness must not change boot security.
- Record the revision, build and qualification result. An advertised HDR EDID alone must not qualify the runner. Repeat qualification after a driver or OS update. Use this trial and the capture trial before estimating the remaining lab work.

### 54. Instrument IDD surfaces and transform generations

Requires: 34, 53.

TODO:

- Record acquired surface bytes, colour metadata, committed wire mode, gamma callbacks and metadata selection. Copy surfaces before ownership expires. Capture the IDD's own transformed output before its encoder.
- Log the callbacks and frames as the records of [IDD callback replay](../design/output_driver/testing.md#idd-callback-replay), in processing order. Log each frame record where the driver takes the transform for that frame, with that transform generation. Verify: `nitpick_model::idd::replay` accepts the logged sequences of a run.
- Verify that each output uses one recorded transform generation, including callback changes during processing. Replay captured input and state against the portable model. A CPU recomputation must not substitute for the driver's transformed artifact.

### 74. Decide the IDD callback semantics that the replay assumes

Requires: 54.

The [IDD callback replay](../design/output_driver/testing.md#idd-callback-replay) uses conservative choices where Microsoft does not define the behaviour.

Open questions:

- Which unit has the fourth column of the 3×4 transform: one scRGB unit (80 cd/m²), 1 cd/m² or another scale? The model takes the unit from `IddContract` without a default. The MHC2 path ignores its fourth column, so Windows can send only zero offsets for a profile. Then the unit stays unverified.
- Is `IDDCX_GAMMARAMP_TYPE_DEFAULT` identity for a driver with the 3×4 transform? The replay takes identity.
- Does an FP16 IDD receive `IDDCX_GAMMARAMP_TYPE_RGB256x3x16`, and in which domain does it apply? The replay takes a LUT in the encoded wire domain.
- Are the 4096 LUT entries at $k/4095$, as in the model, or at another spacing?
- What does the driver apply before the first gamma callback? The replay gives no model for such a frame.
- Can unchanged HDR metadata follow a frame without metadata, or a mode commit? If so, which value applies? The replay rejects both.
- Which primaries has a `G22_P709_WCG` wire? The contract gives them.

TODO:

- With the logged sequences of the qualified runner (54), answer each question. Where the answer differs from the conservative choice, change the replay or the model, and the design section. Verify: the replay accepts the sequences, and `FrameState::model` agrees with the IDD's transformed output within the comparison bound.
- If Windows sends no nonzero offset on the runner, record this and keep the offset unit unverified.

### 55. Verify Windows profile loading through the IDD

Requires: 43, 44, 48, 50, 54, 74.

Open questions:

- Does the selected runner load each new profile without stale calibration? MHC2 activation and legacy gamma-ramp loader settings are separate; qualify the actual transition on this runner.
- Which SDR transfer (sRGB or gamma 2.2) and SDR white does the runner's SDR and SDR WCG wire use? The display model takes both explicitly and has no default. Get them from the committed wire colour space and SDR white of the IDD, or from a measurement, before an SDR comparison.

TODO:

- Assign the known profiles through `DisplayDeviceControl` on the qualified runner. Test HDR and SDR WCG separately, holding other display metadata and controls fixed. Record actual source and wire spaces.
- Check activation through the received transform generation and its effect. A successful profile association or a calibration-loader setting is not proof of activation. Do not use an NVAPI colour-conversion call as an MHC2 loading fallback. Test a missing or stale transform as a qualification failure.
- Compare effective matrix action, sampled curves, raw surfaces and driver output under [Instrumented Windows IDD](../design/output_driver/testing.md#instrumented-windows-idd). Allow recorded callback representation and resampling; reject unexplained transforms. Verify restoration and retain evidence. A pass applies only to that IDD path.

### 56. Add cross-platform execution gates

Requires: 28, 33, 36, 43, 48, 49, 50. Add device jobs after their qualification items pass.

TODO:

- Add the ordinary CI jobs and separate GPU, HDR, calibration and release jobs from [Run matrix](../design/output_driver/testing.md#run-matrix). Run core, driver, fixture and mock protocol checks on Windows, Linux and macOS without a virtual display. Include the `nitpick-cli` process and dependency-graph tests on Linux and macOS without a display server or a window session; they passed only on Windows in a desktop session. Run the `nitpick-profile` adapter tests and the `nitpick-model` tests on each OS; the job needs a C compiler and must not set `LCMS2_LIB_DIR`. The model tests passed only on Windows. Build and unit-test `nitpick-webview` in a separate job on each OS; on Linux it needs the WebKitGTK and GTK development packages, and it has been built only on Windows.
- Run native device query and profile contract tests with injected calls on each target OS. Keep actual profile mutations in selected-device integration jobs. Verify routing and result aggregation with injected pass, fail and unavailable results. A required device losing qualification must fail; an optional unavailable device must not count as HDR coverage. Record conversion, encoding, acknowledgement, capture and settle times separately. Fail bounded-wait and queue-growth checks independently of pixel results.

### 57. Record physical output qualification

Requires: the relevant host and driver checks, the display setup reads of that OS (71 and 73 on Windows, 72 on macOS and Linux), and qualified instruments. Use 51 only for comparisons that include OS capture.

TODO:

- Run the pending QRhi checks in [the measurement plan](test_patterns.md). Add release measurements for browser output, including a remote display, using [Pass criteria](../design/output_driver/testing.md#pass-criteria) and [Usage and restoration](../design/output_driver/testing.md#usage-and-restoration).
- Record wire samples and metadata where a cable exists, repeated probe readings, quantized references, uncertainty, display controls and delay. Check FP16 exceptions and above-peak tone mapping separately. A server capture cannot qualify a remote browser's light output.
- Record actual wire depth and dithering state separately. Check repeated wire samples for temporal changes; one frame cannot establish code preservation. Keep gamma-ramp, GPU-clamp and MHC2 effects isolated under the [display setup](../design/display_device.md#display-setup) baseline.
- Store results at their measured boundary, verify restoration, and update only the claims supported by those results. Remove completed plan items without renumbering the remaining items.
