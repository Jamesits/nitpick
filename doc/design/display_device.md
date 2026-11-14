# Display device operations

## Overview

`DisplayDeviceControl` in `nitpick-core::device` enumerates local display devices, reads their capabilities and current state, and controls their OS settings. The first write operation is ICC profile assignment, including the state needed to restore the previous assignment. `nitpick-display` implements this trait for Windows, macOS and Linux. Both the Qt host and the command-line host can use it.

**Status:** the trait and its types are in `nitpick-core::device`, with a fake-backend contract test. `nitpick-display` has the Windows, macOS and Linux device and capability queries and the recovery-record format. Windows has ICC assignment, readback and restoration; see [Windows profile assignment](#windows-profile-assignment). macOS has the same operations; see [macOS profile assignment](#macos-profile-assignment). They have not run on a Mac. Linux has the same operations through colord; see [Linux profile assignment](#linux-profile-assignment). They have not run on Linux. The remaining work is in [the output driver plan](../plan/output_driver.md).

The host coordinates three separate interfaces:

- [`OutputDriver`](output_driver/overview.md#interface) presents pattern pixels and reports output capabilities.
- `DisplayDeviceControl` enumerates physical display capabilities and reads or changes device profile assignments.
- The [capture adapter](output_driver/testing.md#host-boundary) reads samples for verification.

The [Little CMS profile adapter](output_driver/testing.md#colour-library-boundary) reads profile contents and performs colour calculations. It does not install or assign an OS profile. `nitpick-display` does not evaluate the MHC2 matrix or LUT. OS acceptance of a profile does not prove that its calibration reached the display.

```mermaid
graph LR
    H[Qt host, CLI or test runner] --> O[OutputDriver]
    H --> D[nitpick-core::device<br/>DisplayDeviceControl]
    H --> C[Capture adapter]
    D --> P[nitpick-display]
    P --> W[Windows display and profile APIs]
    P --> M[macOS display APIs and ColorSync]
    P --> L[Linux display APIs and colord]
```

### Crate boundary

| Component | Responsibility | Dependencies |
|---|---|---|
| `nitpick-core::device` | Trait, device identity, capabilities, mode state, profile requests, saved state and errors | Rust data types, with optional `serde` forms; no OS, GUI, executor or colour library |
| `nitpick-display` | Device and capability enumeration, native profile changes, readback and restoration, recovery-record text form | Core plus native bindings selected for the target OS |
| Host or test runner | Device selection, durable recovery record, worker lifetime and test sequence | Core and the selected implementations |
| Profile test support | ICC contents, MHC2 extraction and display model | Little CMS through its separate adapter, `nitpick-profile` |

The core does not import `nitpick-display`. The browser server and optional webview do not import it. The CLI exposes device control as an optional build feature, so a browser-only host still builds and runs without a local display service. The Qt host uses the same native implementations; profile changes do not pass through its C++ presentation bridge.

QRhi reads physical capabilities and mode fields through this trait. Its adapter combines them with swap-chain support and conversion limits to build the output query. Native capability discovery belongs in `nitpick-display`. Qt supplies window geometry and QRhi checks its actual swap-chain support.

## Interface

The following types are owned Rust data. Native values use `Report<T>`: a known value with the name of its native report, or unknown with a reason. A `Report<bool>` converts to the output driver's `Support` tri-state.

| Type | Required meaning |
|---|---|
| `DeviceId` | Backend and native device identity. It is distinct from an output driver's `DisplayId`. |
| `DeviceInfo` | Device ID, name and identity evidence used to detect replacement or ambiguous matches. |
| `DeviceCapabilities` | Detected HDR, WCG, wire-depth and ICC-management support. Keep supported settings distinct from the active settings. Use `Yes`, `No` or `Unknown` with reasons; unknown lists are not empty supported lists. |
| `DeviceMode` | Current HDR state, gamut, wire depth, luminance limits, SDR white, native pixel size and macOS EDR headroom where readable. Unknown values remain unknown. |
| `DeviceQuery` | Capabilities, mode, identity/topology revision and the source of each native report. |
| `ProfileTarget` | Device, native profile role and scope, including user and host scope where applicable. The backend lists the targets it can address. |
| `ProfileState` | Observed associations and selected default or override. Distinguish no override, inherited state and an unreadable value. Include the scope that the OS applies where an OS setting selects it (the Windows per-user switch), and a state revision for change detection. |
| `IccProfile` | Owned profile bytes and their SHA-256 content hash. A caller's path is not the profile identity. Construction checks only the ICC header size and `acsp` signature. |
| `PreparedProfileChange` | Versioned device identity, target, original state, desired profile and planned resources owned by this change. It can be saved for recovery before any mutation. |
| `DeviceError` | Unsupported operation or scope, permission denied, invalid profile, device disconnected, ambiguous identity, state conflict, timeout or native error. Retain the native reason. |

Pseudo-code (Rust):

```rust
pub trait DisplayDeviceControl {
    fn devices(&mut self) -> Result<Vec<DeviceInfo>, DeviceError>;
    fn query_device(
        &mut self, device: &DeviceId,
    ) -> Result<DeviceQuery, DeviceError>;
    fn profile_targets(
        &mut self, device: &DeviceId,
    ) -> Result<Vec<ProfileTarget>, DeviceError>;
    fn profile_state(
        &mut self, target: &ProfileTarget,
    ) -> Result<ProfileState, DeviceError>;
    fn prepare_icc_profile(
        &mut self, target: &ProfileTarget, profile: &IccProfile,
    ) -> Result<PreparedProfileChange, DeviceError>;
    fn set_icc_profile(
        &mut self, change: &PreparedProfileChange,
    ) -> Result<ProfileState, DeviceError>;
    fn restore_profile(
        &mut self, change: &PreparedProfileChange,
    ) -> Result<ProfileState, DeviceError>;
}
```

Device queries are read-only. Query again before opening output or preparing a device change, and after a display-state notification. Return partial mode information when a field is unavailable. Do not infer active HDR from generic Advanced Color support, a panel's advertised peak, or an FP16 storage format.

Device capabilities do not describe the output transport. Accepted pattern encodings, 10/12-bit input paths, FP16 conversion limits, pixel mapping, metadata transport and code preservation remain in the output driver's `Capabilities`. Physical fields in QRhi `ModeSettings` come from `DeviceQuery`; the output adapter adds its presentation description and pixel ratio. The browser supplies its own client reports and unknown fields. It does not query a server display to fill gaps in a remote client's report.

`prepare_icc_profile` reads and validates state without changing it. It rejects a target whose original assignment cannot be read or restored; `ProfileState::check_restorable` is the shared check. A record of another format version fails with `Unsupported`. The host saves the prepared change before calling `set_icc_profile`. The backend rechecks device identity, scope and state before a write. A conflicting external change returns `StateConflict`; it must not redirect the write to another display.

Setting a profile includes installation or registration when the OS requires it, association with the selected device and selection in the requested role. Use an existing identical profile when possible. Read back the requested state before reporting success. Native resources created for the change have unique planned identities. Partial failure still requires restoration from the saved record.

Restoration removes the override when the original state had none. It restores the previous selection and associations otherwise. It removes only resources created by the change and no longer used. User profiles and unrelated assignments remain intact. Repeated restoration is safe. If another actor changed the device state, preserve the recovery record and report a conflict instead of overwriting that state.

Success means the OS profile assignment matches the request. Calibration activation is a separate [verification check](output_driver/testing.md#instrumented-windows-idd). Profile state does not certify HDR rendering, MHC2 execution, wire values or panel light.

### Identity and execution

Screen order, display names and EDID hashes alone are not unique device keys. A backend maps its native identity to the current display topology and revalidates it before each change. Reconnection can require a new mapping. Ambiguous or disconnected targets fail without a write.

The host maps a local QRhi screen to a device through native identity. A browser client ID does not identify a device on the server. An explicit local-device selection is required for a local browser test. Remote browser profile control is unsupported by this interface; the host must not change its own monitor as a substitute.

Trait calls can perform native I/O. Hosts run blocking work on a worker and serialize changes for a device. Backends handle native thread requirements without depending on Qt. Cached query results can serve the synchronous output query; refresh them before opening a surface. Waiting must not block the Qt GUI thread or CLI event loop. Dropping a host wait or reaching its timeout does not undo an OS write. Keep the worker and recovery record until the outcome is known, then query and restore as needed. Do not start a replacement mutation while the old operation can still write.

Before a profile change, the host cancels pending presentation and measurement timing. After the assignment is read back, it queries the output again, opens a new surface if needed and submits a new frame. Old callbacks, captures and ready events cannot qualify that frame. The measurement delay still starts after the new presentation acknowledgement.

## Platform implementations

| Platform | Selected implementation | Qualification needed |
|---|---|---|
| Windows | Native display queries through DXGI and DisplayConfig, with Advanced Color queries where needed. Native profile installation and [display association](https://learn.microsoft.com/en-us/windows/win32/api/icm/nf-icm-colorprofileadddisplayassociation) APIs, with explicit scope and [default profile role](https://learn.microsoft.com/en-us/windows/win32/api/icm/nf-icm-colorprofilesetdisplaydefaultassociation) | Resolve display identity across the query and profile APIs. Keep supported HDR, active HDR and SDR WCG distinct. Keep ordinary and Advanced Color profile roles distinct. Check assignment and restoration in both modes. |
| macOS | Native display identity and available EDR reports; [ColorSync device profile assignment](https://developer.apple.com/documentation/colorsync/colorsyncdevicesetcustomprofiles(_:_:_:)) | Map the display to its ColorSync device UUID and profile ID. Keep EDR headroom separate from absolute luminance. Save user and host scope. Restore a prior custom profile or remove its override. |
| Linux | DRM connectors in sysfs and KMS connector properties; [colord device profile association and default selection](https://www.freedesktop.org/software/colord/gtk-doc/CdDevice.html) | Check service availability, authorization and persistence. Establish separately whether the compositor uses the assigned profile. |

Each platform module maps native reports to the core types through an injected native-call trait. Only the module that calls the OS is limited to its target, so the mapping tests run on every OS.

### Windows reports

The device key is the monitor device interface path from `DISPLAYCONFIG_TARGET_DEVICE_NAME`. It survives a restart and distinguishes identical monitors. The identity evidence holds only fields that survive a restart: EDID IDs, connector instance and output technology. The backend re-resolves the adapter LUID and source ID from the key before each native call.

| Field | Source | Rule |
|---|---|---|
| `hdr`, `sdr_wcg` support | `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2` (Windows 11 24H2 and later) | Without it, the legacy support bit covers both kinds. A kind is then known only when it is active, or when no Advanced Color is supported. |
| `hdr_on`, active kind | `INFO_2` active colour mode; otherwise the legacy enabled bit and the DXGI PQ colour space | The legacy enabled bit alone does not set HDR. |
| `gamut` | Active kind | HDR is BT.2020; plain SDR is sRGB; SDR WCG is unknown. |
| `wire_depth` | `bitsPerColorChannel` of either Advanced Color report | 0 is unknown. Supported depths are unknown: Windows reports the active depth only. |
| Luminance | `DXGI_OUTPUT_DESC1`, matched by GDI source name | 0 is unknown. A minimum is known only with a known peak. |
| `sdr_white_nits` | `DISPLAYCONFIG_SDR_WHITE_LEVEL` | HDR active only. |
| `native_size` | Active signal size of the target mode | |

**Unverified:** whether DXGI substitutes OS default luminance for an EDID without HDR static metadata, whether the legacy wide-colour bit is set only for SDR WCG, and the wire gamut of SDR WCG. A check on Windows 11 build 26200 with one HDR display read every field above, with HDR on at 8 bits per channel.

`icc_profiles` is `Yes` when `mscms.dll` exports the `ColorProfile*` functions (Windows build 20348 and later), and `No` otherwise.

### Windows profile assignment

`nitpick_display::windows` uses the [display association functions](https://learn.microsoft.com/en-us/windows/win32/api/icm/nf-icm-colorprofileadddisplayassociation) of `mscms.dll`. They address a display by the adapter LUID and source ID, which the backend re-resolves from the device key before each call. The backend loads every `mscms.dll` function at run time.

Each device has four targets: the ordinary and the Advanced Color role, each in the current-user and the system-wide scope. Windows has no host scope; a target with a host scope is `Unsupported`.

Native model. Items marked "observed" come from a check on Windows 11 build 26200 and are not in the Microsoft reference:
- Each scope and role has an association list and a default.
- A per-user switch ("Use my settings for this device") selects the scope that Windows applies. `ColorProfileGetDisplayUserScope` reads it. The only setter is the legacy `WcsSetUsePerUserProfiles`, which takes the legacy device name `MONITOR\<id>\{class}\<n>` from `EnumDisplayDevicesW`. Observed: a current-user association does not set the switch.
- Observed: the first association of an empty list becomes its default without `setAsDefault`. Removing the only association leaves no default.
- Observed: `ColorProfileGetDisplayDefault` returns `ERROR_FILE_NOT_FOUND` when there is no default. The current-user default reads back while the switch is off. **Unverified:** whether a current-user read without a current-user default falls back to the system-wide default; the mapping assumes it does not.
- Observed: as a standard user, system-wide association and default calls returned success without a change. The readback then fails with `StateConflict`.

| `ProfileState` field | Source | Rule |
|---|---|---|
| `associations` | Registry value `ICMProfile` (ordinary) or `ICMProfileAC` (Advanced Color) | Current user: `HKCU\Software\Microsoft\Windows NT\CurrentVersion\ICM\ProfileAssociations\Display\{class}\<n>`. System-wide: the monitor driver key `HKLM\SYSTEM\CurrentControlSet\Control\Class\{class}\<n>`. A missing key or value is an empty list. |
| `selection` | `ColorProfileGetDisplayDefault` with `CPST_STANDARD_DISPLAY_COLOR_MODE` or `CPST_EXTENDED_DISPLAY_COLOR_MODE` | `Override` if the target scope has a default. For a current-user target without one, `Inherited` from the system-wide default when the switch is off; otherwise `NoOverride`. |
| `applied_scope` | `ColorProfileGetDisplayUserScope` | A read failure makes the selection unreadable. |
| Profile hash | File in the colour directory | Unknown if the file is missing. |

**Deviation:** the lists come from undocumented registry values. `ColorProfileGetDisplayList` returned no profile in either scope while both roles had current-user associations, with the switch on and off. The current-user values are observed. `ICMProfile` in a monitor driver key was observed for another monitor; `ICMProfileAC` there is **unverified**.

`prepare_icc_profile` selects the profile file in this order:
1. An association of the target with the same content. The change only selects it. If the target has no selection, preparation fails, because restoration would need to remove the user's association.
2. A file with the same content in the colour directory, except files with the `nitpick-` prefix. The change adds an association.
3. A new file `nitpick-<profile hash>-<target hash>.icc`. The change installs it and adds an association. If the file already exists, preparation fails with `StateConflict`: an earlier change did not finish.

The planned resources name the file. Restoration removes only a file with the `nitpick-` prefix that the record plans.

```mermaid
stateDiagram-v2
    [*] --> Checked: identity, topology and state equal the record
    Checked --> Installed: InstallColorProfileW (new file only)
    Installed --> PerUser: WcsSetUsePerUserProfiles(on) (current-user target with the switch off)
    PerUser --> Associated: ColorProfileAddDisplayAssociation (new association only)
    Associated --> Selected: ColorProfileSetDisplayDefaultAssociation
    Selected --> [*]: readback has the selection, the association and the applied scope
```

`restore_profile` rechecks the device identity, but not the topology. It accepts only a current state that a prefix of these steps gives from the original state; another state is `StateConflict`. It then removes the planned association, restores a previous default, restores the per-user switch and uninstalls the planned file if no display lists or selects it. The readback must equal the original state. Association order is not compared: Windows does not document its meaning.

**Deviation:** restoration leaves the current-user registry key with empty lists and the switch off when the key did not exist before. The Windows functions then report the original state. **Open:** whether restoration must remove this key.

A check on Windows 11 build 26200 with one HDR display (HDR on, per-user switch off, no prior association) assigned `ordinary_srgb` in both roles and `ordinary_p3_gamma22` in the Advanced Color role, in the current-user scope, read back each assignment and restored the original state. The functions accepted `ordinary_p3_gamma22`. This shows only the association; it does not show that SDR colour management uses the profile. The system-wide target failed at readback as a standard user and was restored. **Unverified:** the ordinary role with HDR off and in SDR WCG, a system-wide assignment as an administrator, a display with existing associations, a system-wide list that is not empty when the switch changes, and the legacy device name with more than one monitor on a GDI source.

### macOS reports

The device key is the ColorSync display device UUID from `CGDisplayCreateUUIDFromDisplayID`. ColorSync profile assignment uses this UUID with the display device class. The `CGDirectDisplayID` changes after a reconnect; it only links the Core Graphics, ColorSync and `NSScreen` reports in one query. Without a UUID, the key is the display ID. The identity evidence is the vendor and model numbers, the EDID serial number and the built-in flag.

| Field | Source | Rule |
|---|---|---|
| Name | `NSScreen.localizedName`; otherwise the ColorSync device description | |
| `hdr`, `hdr_on` | none | Unknown. EDR headroom does not establish HDR: macOS can give headroom on an SDR panel at reduced brightness. |
| `sdr_wcg` support | `NSScreen.canRepresentDisplayGamut(P3)` | |
| `gamut` | none | Unknown. macOS converts to the display profile and does not report the signal gamut. |
| `wire_depths`, `wire_depth` | none | Unknown. |
| Luminance, `sdr_white_nits` | none | Unknown. EDR headroom does not fill these fields. |
| `native_size` | Pixel size of the mode with `kDisplayModeNativeFlag`, from `CGDisplayCopyAllDisplayModes` with the duplicate low-resolution modes | Not the current mode: a scaled mode has a framebuffer of another size. No flagged mode or more than one flagged size is unknown. |
| `edr` | Current, potential and reference EDR component values of the `NSScreen` | A ratio to SDR white = 1.0, not a luminance. A value below 1 or not finite is unknown. A reference value of 0 means no reference preset. |
| `icc_profiles` | `ColorSyncDeviceCopyDeviceInfo` | No if ColorSync has no device for the UUID. Yes if the device has one profile ID for assignment; see [macOS profile assignment](#macos-profile-assignment). Otherwise unknown. |

Core Graphics and ColorSync calls run on the calling worker. `NSScreen` is main-thread only. A call on another thread goes to the main dispatch queue and waits at most 2 s. The main queue runs only when the main thread runs a run loop, as in the Qt host. Without it, or after `dispatch_main`, the name falls back and the `NSScreen` fields are unknown with the reason. The other fields remain.

**Unverified:** that the UUID survives a restart, whether two identical monitors without EDID serial numbers get the same UUID (the backend then reports an ambiguous identity), whether every panel flags its native mode, whether the P3 report follows the panel or the assigned profile, and whether `NSScreen` updates after a topology change in a process without `NSApplication`. A check on macOS 27.0.1 with a MacBook Pro (M1 Max) built-in XDR display read every field above. It reported P3 support, native size 3024 × 1964, EDR headroom 1.2 current and 16 potential, no reference preset, and ColorSync default profile ID 1. No external display was checked.

### macOS profile assignment

`nitpick_display::macos` uses [`ColorSyncDeviceSetCustomProfiles`](https://developer.apple.com/documentation/colorsync/colorsyncdevicesetcustomprofiles(_:_:_:)) with the display device class and the device UUID. ColorSync keeps one custom profile file for each device profile ID and preference scope. A scope has a user part (current user or any user) and a host part (current host or any host). ColorSync has no Advanced Color role, so each target has the `Standard` role and a host scope.

The profile ID is the default profile ID of the factory profiles, or the only factory profile ID. `ColorSyncDevice.h` makes the default ID optional for a device with one profile. Without one profile ID, the device has no targets. The device has one target for each scope that is not wider than its registration scope (`kColorSyncDeviceUserScope` and `kColorSyncDeviceHostScope`). `ColorSyncDevice.h` states that a custom profile scope cannot be wider than the device scope. A missing registration value is the current user or the current host, the `ColorSyncRegisterDevice` default.

Native model:
- The scopes resolve in the `CFPreferences` search order: current user and current host, current user and any host, any user and current host, any user and any host. The first scope with an entry applies. **Unverified** for ColorSync; the state read rejects a state that does not agree with this order.
- `kCFNull` in place of the file URL reverts the scope to the factory profile (`ColorSyncDevice.h`). **Unverified:** whether this removes the entry, so that a wider entry applies again. The restoration readback detects the other result.
- The any-user scope needs root, as the `customdisplayprofiles` administrator tool states. The backend returns `PermissionDenied` before a write without root. **Unverified:** what a call without root does.

| `ProfileState` field | Source | Rule |
|---|---|---|
| `associations` | `CFPreferencesCopyValue` of the target scope: global domain, key `com.apple.ColorSync.Devices`, entry `Device.mntr.<UUID>`, dictionary `CustomProfiles` | The entry of the profile ID: zero or one file path. A missing key is no entry. |
| `selection` | The same entries of all four scopes, and `kColorSyncCustomProfiles` of `ColorSyncDeviceCopyDeviceInfo` | `Override` if the target scope has an entry. Otherwise `Inherited` from the first wider scope with an entry, or `NoOverride`. `Unreadable` if the first entry in resolution order is not the `ColorSyncDeviceCopyDeviceInfo` value. |
| `applied_scope` | none | Always `None`: no macOS setting selects the scope. |
| Profile hash | The file | Unknown if the file is missing. |
| `revision` | All four scopes and the resolved value | A change at a narrower scope also changes it. |

**Deviation:** the entries of each scope come from undocumented preference storage. `ColorSyncDeviceCopyDeviceInfo` gives only the value resolved for the current user and host. The storage layout is from administrator reports for older macOS versions; it is **unverified** on current macOS. If the layout is different, the state is unreadable and preparation fails; the state cannot be wrong without detection.

`prepare_icc_profile` reads the state without a change, and then:
1. It rejects a target when a narrower scope has an entry, because the assignment would not apply.
2. It checks root for the any-user scope.
3. `ColorSyncProfileCreate` and `ColorSyncProfileVerify` must accept the profile. Otherwise it returns `InvalidProfile`.
4. If the target entry has the same content, the change has no resources and writes nothing.
5. Otherwise it plans a new file `nitpick-<profile hash>-<target hash>.icc` in `~/Library/ColorSync/Profiles` (current user) or `/Library/ColorSync/Profiles` (any user), and an entry for the profile ID. The planned resources are the file path (`InstalledProfile`) and the profile ID (`Association`). If the file exists, preparation fails with `StateConflict`.

**Deviation:** the backend does not use an identical file in another location, as the Windows backend does. See [Technical decisions](#technical-decisions).

```mermaid
stateDiagram-v2
    [*] --> Checked: identity, topology, profile ID and state equal the record; root for any user
    Checked --> Installed: create the planned file
    Installed --> Set: ColorSyncDeviceSetCustomProfiles(file URL)
    Set --> [*]: readback has the file at the target, with the profile hash, and no narrower entry
```

`restore_profile` rechecks the device identity, but not the topology. It uses the profile ID of the record, also if the device now has another ID. It accepts only a state in which the target holds the original entry or the planned file, and the wider entries are the same as in the original state; another state is `StateConflict`. It then sets the original entry, or `kCFNull` if there was none. It removes the planned file if no entry of a current display names it. It cannot read the current-user entries of other users. The readback must have the original associations and selection.

Profile operations do not read `NSScreen`. The device name in their records is the ColorSync description.

**Unverified:** all of this section on a Mac. A successful assignment shows only the ColorSync state. It does not show that the compositor uses the profile, MHC2 execution or HDR luminance.

### Linux reports

The backend reads the kernel display state, not a compositor report. The devices are the connectors in `/sys/class/drm` with the status `connected`. A device list reads only sysfs. Profile operations also read colord, but open no card node. A query also opens the card node `/dev/dri/cardN` of that device to read its KMS connector properties, and reads the display devices of colord. The KMS reads need seat access to the card node. Without access, the KMS fields are unknown with the reason, and the EDID fields remain.

The device key is the bus ID of the card device, the kernel connector name and the EDID identity, for example `0000:01:00.0/DP-6/DEL40E7-<serial>`. The EDID identity is the PNP ID, the product code, and the serial number descriptor or else the numeric serial number. Another unit on the same connector, or the same unit on another connector, gets a new key. Without a readable bus ID, the key holds the card number, which can change after a restart. The identity evidence is the connector name, the EDID IDs, both serial number forms and the EDID MD5.

The colord device of an output is the display device whose `OutputEdidMd5` metadata is the MD5 of the connector's EDID. Only devices of the calling user count. Two outputs with the same EDID, or two matching devices, make the match ambiguous.

The Qt host maps a `QScreen` to a device as follows:

- Wayland: `QScreen::name()` is the compositor's output name. The compositors write the kernel connector type and number. KWin and wlroots use the kernel type names; GNOME writes the type `HDMI-A` as `HDMI`, for example `HDMI-2` for `HDMI-A-2`. The screen maps to the one connected device whose connector name is the screen name, or is `HDMI-A-n` for the screen name `HDMI-n`. The names do not include the card, so the same name on two cards is ambiguous.
- X11 through Xwayland: the RandR output name is the compositor's output name, with the same rule. Xwayland outputs have the RandR property `RANDR Emulation` and no `EDID` property.
- Other X servers: the output names come from the X driver and can differ from the kernel names. The screen maps to the one device whose EDID MD5 is the MD5 of the RandR `EDID` output property. An output without that property maps to no device.

`nitpick_display::linux::screen_device` implements these rules on a device list. A screen without exactly one match maps to no device. Its device fields are then unknown with the reason, and output continues with warnings. The Qt manufacturer and model strings are not compared: GNOME gives the PNP ID and the EDID product name, while KWin and wlroots give the vendor name from the PNP database where they find it.

| Field | Source | Rule |
|---|---|---|
| Name | EDID display product name | Otherwise the connector name. |
| `hdr` support | SMPTE ST 2084 bit of the EDID HDR Static Metadata Data Block; the KMS property `HDR_OUTPUT_METADATA` | `No` if either reports no. A DisplayID extension without the CTA-861 block is unknown. Compositor support is not reported. |
| `hdr_on` | `HDR_OUTPUT_METADATA` blob | On for a PQ or HLG EOTF. Off for blob 0 or without the property. |
| `sdr_wcg` support | none | Unknown. |
| `gamut` | KMS `Colorspace` | Known for the BT.2020 and DCI-P3 D65 values only. `Default` signals no colorimetry, and a colour-managed compositor can send the panel gamut. |
| `wire_depths`, `wire_depth` | none | Unknown. `max bpc` is a limit, not the link depth. |
| Luminance | Desired content max, max frame-average and min luminance of the EDID HDR Static Metadata Data Block | Peak, full frame and minimum. Code 0 of a maximum is unknown. A minimum is known only with a known peak. |
| `sdr_white_nits` | none | Unknown. Compositor settings are not read. |
| `native_size` | Mode of the CRTC of the connector | No CRTC is unknown. |
| `icc_profiles` | colord `GetDevicesByKind` | Yes if one colord device matches. No if colord runs and no device matches. Otherwise unknown with the reason. The query does not start colord. |

The first process that opens a card without a DRM master becomes its master. The backend drops the master immediately, so a compositor that starts during the query can take the card.

**Unverified:** that code 0 of a maximum luminance means "not given", that every driver signals no colorimetry for `Default`, that the wire size is the CRTC mode on an internal panel with a GPU scaler, and that colour daemons other than the GNOME daemon set `OutputEdidMd5`. A check on Ubuntu 26.04 (kernel 7.0, GNOME 50.1 on Wayland, NVIDIA driver 595.99.02, colord 1.4.8) with an SDR monitor on DisplayPort and an HDR10 recorder on HDMI read every field above. With HDR on in GNOME, the HDMI output reported `hdr_on`, BT.2020 and 3840 × 2160; its EDID has no luminance codes. Each output matched one colord device. With Qt 6.12, both Wayland screens gave the GNOME names `DP-6` and `HDMI-2`, the PNP ID and the product name, and no serial number. Through Xwayland they gave the same names and no EDID. The KWin and wlroots names are from their source code; Qt and Xwayland with these compositors were not checked. X servers other than Xwayland, and other drivers, were not checked.

### Linux profile assignment

`nitpick_display::linux` uses the colord D-Bus interface `org.freedesktop.ColorManager`. The colord device of an output is the device that [Linux reports](#linux-reports) match. A device without exactly one match has no targets. A missing colord service, a missing match or an ambiguous match does not stop the device list or the query. Each device has one target: the `Standard` role in the current-user scope without a host part. colord has no Advanced Color role and no scope setting.

Native model. It comes from the colord 1.4.9 source (`cd-device.c`, `cd-main.c`, `cd-mapping-db.c`, `cd-profile-array.c`) and is not in the colord reference:
- A device has an ordered profile list. Hard relations come first, then soft relations. In each relation, the newest entry comes first. The first entry is the default profile.
- `AddProfile` with the relation `hard` and `MakeProfileDefault` give the entry the current time. They do not change other entries. colord stores a hard relation in its mapping database, by device ID and profile ID.
- `RemoveProfile` sets the timestamp of the database row to 0. It does not remove the row, so colord never adds that profile to the device again from metadata.
- A new profile object gets each device that has a database row with a nonzero timestamp for its ID.
- colord accepts only the profile scopes `temp` and `normal`. A `normal` profile remains until `DeleteProfile` or a colord restart. `DeleteProfile` also removes the profile from every device.
- `FindProfileById` and the duplicate check of `CreateProfileWithFd` prefer a profile of the calling user, and otherwise find a profile of any owner.
- polkit authorizes each write. The colord policy allows the writes in an active local session and requires an administrator otherwise.

| `ProfileState` field | Source | Rule |
|---|---|---|
| `associations` | `Profiles` of the colord device; `ProfileId` and `Filename` of each profile | In colord order. The native ID is the colord profile ID. |
| `selection` | The first entry | `Override`; `NoOverride` for an empty list. colord has no selection separate from the list. |
| `applied_scope` | none | Always `None`. |
| Profile hash | The `Filename` file | Unknown without a file, or if the file cannot be read. |
| `revision` | The list, with the relation (`GetProfileRelation`), object path and owner of each entry | A change of a relation also changes it. |

`prepare_icc_profile` reads the state without a change. If the default profile has the same content, the change has no resources and writes nothing. Otherwise it plans these resources:
- `InstalledProfile`: the file `$XDG_DATA_HOME/nitpick/icc/nitpick-<profile hash>-<target hash>.icc`.
- `RegisteredProfile`: the colord profile ID `nitpick-<profile hash>-<target hash>`.
- `Association`: the colord device ID.

If the file exists, or colord has a profile with the ID, preparation fails with `StateConflict`.

**Deviation:** the backend does not use an identical profile that colord has already. See [Technical decisions](#technical-decisions).

The file is not in `$XDG_DATA_HOME/icc`, because session daemons register each file there as one more profile object. colord reads the profile through a file descriptor: it cannot read home directories. A compositor reads the `Filename` path.

```mermaid
stateDiagram-v2
    [*] --> Checked: identity, topology, colord device ID and state equal the record
    Checked --> Installed: create the planned file
    Installed --> Registered: CreateProfileWithFd(scope normal, Filename)
    Registered --> Associated: AddProfile(hard), if colord did not add it from its database
    Associated --> Default: MakeProfileDefault
    Default --> [*]: readback has the planned profile first, with the profile hash
```

`restore_profile` rechecks the device identity and the colord device ID, but not the topology. It accepts only the original list with the planned profile at one position or absent; another list is `StateConflict`. It then removes the planned profile from the device. It deletes the colord profile if the calling user owns it and no other visible device lists it. It removes the file, unless the profile remains. The readback must have the original associations and selection.

**Deviation:** restoration leaves one database row with timestamp 0 for each profile and target. colord has no call that removes a row. A repeated change of the same profile and target uses the same ID, so the rows do not accumulate. After a colord restart, the registered profile is gone, but its row can keep a nonzero timestamp. A later change with the same ID then gets the association from colord and does not add it.

**colord defect** (source): `CreateProfileWithFd` adds the profile to its list before it reads the file. If the read fails, the profile stays in the list without a D-Bus object until colord restarts. `FindProfileById` then gives an object path that does not answer, and restoration fails until a colord restart.

**Unverified:** all of this section on Linux, and the native model on colord versions other than 1.4.9. Also unverified:
- Which compositors apply the colord default profile, and whether they read `Filename`. KWin has its own output ICC setting.
- Whether a colord restart reorders soft relations. They get the time of the automatic addition. Restoration reports a reorder as `StateConflict`.
- What the session of another user gets. colord stores the relation by device ID, so a device of another user with the same ID gets it, if that session can see the profile.

A successful assignment shows only the colord state.

**Unverified:** these implementations and their restoration behaviour on the selected OS versions. Search existing Rust bindings before adding native declarations. Pin the chosen dependencies. Use only the bindings needed on the target OS.

The crate must build on all three operating systems. Missing APIs, services, roles or permissions return explicit errors or unknown capability fields. Linux without a usable colord display mapping has no profile setter in this design; display enumeration can still work. A successful colord assignment does not establish compositor application. A successful ColorSync assignment does not establish a Windows MHC2 path. Keep these limits separate from build support.

## Technical decisions

| Decision | Options | Selected and reason |
|---|---|---|
| Device queries and writes | Put native operations in each output driver; use a separate trait | Separate trait. Physical capabilities and settings are shared across presentation paths. |
| Implementation location | Qt bridge; test scripts; native Rust crate | `nitpick-display`. The same implementation serves both hosts and tests. |
| Profile contents | Parse inside the native backend; separate colour adapter | Separate adapter. OS assignment and colour evaluation have different dependencies and tests. |
| Profile selection | One unqualified path; explicit native role and scope | Explicit target. The OS can have different defaults and overrides for one device. |
| Call model | Native work in the UI; synchronous trait on a worker | Worker. Native APIs can block, and cancellation must not hide an unfinished write. |
| Recovery | Save after assignment; prepare and save before assignment | Prepare first. A partial native failure or process crash must leave enough state to recover. |
| Recovery-record format | Separate record types in `nitpick-display`; `serde` forms of the core types | `serde` behind a core feature: one definition of each type. The reader checks `version` before other fields, because another version can give a field another meaning. Profile bytes and hashes are hex text, and the stored hash must match the bytes. |
| Native-call tests | Test only on each OS; inject native reports | Inject. The mapping compiles on every OS, and unknown or failed fields can be tested without a display. |
| macOS EDR headroom | Put it in the luminance fields with an assumed SDR white; a separate ratio field | Separate `edr` field. The SDR white luminance changes with brightness and is not reported. An assumed scale would give false absolute values and a false exact claim for the QRhi FP16 path. |
| macOS `NSScreen` thread | Require the main thread; synchronous dispatch to the main queue; asynchronous dispatch with a bounded wait | Bounded wait. The trait runs on a worker. A synchronous dispatch blocks for ever in a host without a main run loop. A late block sends to a closed channel. |
| Linux report source | Wayland output reports; XRandR; DRM sysfs and KMS properties | DRM sysfs and KMS. KMS holds the signalled HDR state and colorimetry for any compositor, and it works without a window system. A Wayland client needs a session connection, and the colour-management protocol is optional and gives the compositor's output description. GNOME 50 Xwayland gives no EDID through RandR. Cost: the KMS fields need seat access to the card node. |
| Linux Qt screen mapping | Qt manufacturer and model; kernel connector name; EDID MD5 | Connector name, with the GNOME form of `HDMI-A`, on Wayland and Xwayland; EDID MD5 on other X servers. Wayland gives no EDID and no serial number to clients, and the manufacturer and model formats differ between compositors. X driver output names can differ from the kernel names, but a real X server gives the EDID. |
| Linux profile-service mapping | Connector name; colord device ID; EDID MD5 | EDID MD5 in the colord device metadata. Connector names differ between the kernel and compositors. The colord device ID is made by the session daemon and does not name the connector. |
| Topology revision | std hasher; fixed hash | FNV-1a 64 over sorted identity fields. Recovery records store the value, so it must not change between builds or runs. |
| Applied scope in the profile state | Restore lists and defaults only; add a field | Field `applied_scope`. A current-user assignment must set the Windows per-user switch, or Windows ignores it. Without the field, restoration cannot return the switch, and "no override" with the switch on equals "no override" with it off. |
| Windows association lists | `ColorProfileGetDisplayList`; registry values; no list (role unsupported) | Registry values. The list function returned no profile while associations existed, so restoration could not detect or remove an association. Cost: undocumented storage; a changed layout gives wrong lists, which the readback detects as a mismatch. |
| Windows `mscms.dll` binding | Static import; run-time loading | Run-time loading. A static import of the `ColorProfile*` functions stops the process from loading before Windows build 20348. |
| Windows installed profile file | Use the caller's name; content hash; content hash and target | Content hash and target, with the `nitpick-` prefix. The name is unique per target, so two changes never share a file, and restoration cannot remove a user's file. |
| macOS custom profile scopes | Only the resolved `ColorSyncDeviceCopyDeviceInfo` value, with one target; undocumented preference read of each scope, checked against the resolved value | Preference read with the check. The resolved value cannot show if an entry is at the target or at a wider scope, so restoration could add or remove the wrong entry. Cost: undocumented storage; a wrong read makes the state unreadable, not wrong. |
| macOS profile file | Use an identical file in a profile folder; always install a new file | A new file with the Windows name rule, unless the target entry already has the same content. A planned resource holds one value, and the record must name both the file and the profile ID. A new file also makes the cleanup the same for each change. |
| Linux profile registration | Copy the file into `$XDG_DATA_HOME/icc` and wait for a session daemon to register it, as `colormgr import-profile` does; `CreateProfileWithFd` with a file in another directory | `CreateProfileWithFd`. The result does not depend on a session daemon or a wait. A file in the monitored directory gives a second profile object. |
| colord profile scope | `temp`; `normal`; `disk` | `normal`. colord removes a `temp` profile when the D-Bus connection of the call closes, and each native call opens its own connection. colord rejects `disk` for profiles. Cost: a colord restart removes the profile; restoration then removes only the file. |
| Linux identical profile | Use an identical colord profile; always register a new profile | A new profile, unless the default profile already has the same content. A hard addition of a profile of the user changes a soft relation to hard, and its removal leaves a database row with timestamp 0 for that profile. |

## Usage

Select a local device and a supported profile target. Use the current user scope where supported; other scopes must be explicit. Prepare the change and persist its recovery record before assignment. Confirm the OS state, then run the presentation and measurement sequence. Restore and confirm the original state at the end of the run.

On startup, inspect unresolved recovery records before changing that device again. An unreadable or incompatible record fails recovery with a reason. Keep it for investigation. Follow [test setup and restoration](output_driver/testing.md#usage-and-restoration) for other display controls.

## Testing

Run the same trait contract against a fake backend and each native backend. To print the readable fields of each local display, run `cargo run -p nitpick-display --example devices`. Ordinary CI uses injected native calls and does not change the developer's display profile. Test capability tri-states, supported versus active modes, partial query data, stale caches and profile queries without write permission. Test device replacement, duplicate display names, role and scope selection, no prior override, wrong profile bytes and unsupported services.

Inject failure after installation, association and default selection. Verify recovery after each step, timeout, cancellation and process restart. Test concurrent external changes, shared pre-existing profiles and repeated restoration. A completed assignment must have matching readback. Failed restoration fails the run.

Native integration tests use a selected test display and the `ordinary` [profile fixtures](output_driver/testing.md#profile-fixtures). Run assignment, readback and restoration on each supported OS. The example below selects the one device whose key contains `<key part>`, saves the recovery record before the first write, assigns, restores and prints each state. It accepts only `ordinary` fixtures. A macOS target needs the host part of the scope; a Windows target must not have it. The Linux target is `standard user`. After a failed restoration it keeps the record; `restore` retries from it:

```text
cargo run -p nitpick-display --example profile_roundtrip -- state <key part>
cargo run -p nitpick-display --example profile_roundtrip -- assign <key part> <standard|advanced> <user|system>[/<this-host|any-host>] <fixture id> <record file>
cargo run -p nitpick-display --example profile_roundtrip -- restore <record file>
```

Apply MHC2 fixtures only in the Windows calibration tests. Confirm calibration activation and light output at their own boundaries in [Output verification](output_driver/testing.md).

## Future enhancements

- HDR mode, output depth, brightness and vendor LUT controls through explicit device operations. These setters are outside the current ICC assignment scope.
- Remote device agents with their own identity and authorization contract.
- Additional Linux profile services after their assignment and restoration contracts are defined.
