# Plan: HDR/WCG test patterns

## Windows

### 1. Verify the signal with a vectorscope

Open questions:
- Does DWM keep the code values without change (independent flip), or does composition change them by one code?
- Does the GPU driver use the BT.2020 matrix and narrow range when it converts to YCbCr?

TODO:
- Set the GPU output to 10 bpc RGB full range. Show `bt2111` with `pq-narrow`. Compare each patch on the scope with the reference table. Done when every patch is within ±1 code.
- Do the test again with YCbCr 4:4:4 narrow range and `pq-full`.
- Write the results in `doc/design/output_driver/qt.md` (Testing) and remove the "Not done" statement.

### 2. Verify the 12-bit output

Open questions:
- Does the compositor or the driver decode FP16 scRGB with the same matrix as the reference decoder in `output.rs`?

TODO:
- Set the GPU output to 12 bpc RGB full range. Show `bt2111` and `pq-grayscale` with 12-bit. Compare with the 12-bit reference table. Done when every level without `*` is within ±1 code.
- Write the results and the deviations of the `*` levels in `doc/design/output_driver/qt.md` (Swap chain output, Testing).

### 3. Verify the HDR10 metadata

Open questions:
- Which unit does the driver expect for DXGI `MaxMasteringLuminance`: whole cd/m² or 0.0001 cd/m²?

TODO:
- Capture the HDMI/DisplayPort HDR InfoFrame (for example with an analyser that shows it) with metadata on. Done when the InfoFrame shows the values of the launcher.
- Do the test again with metadata off. Done when the InfoFrame has no static metadata, or the driver default.

## Linux

### 4. Verify pattern and HDR10 metadata

- Run on a Wayland compositor with HDR (for example KDE Plasma 6). Done when both swap chains show both patterns and the metadata status shows "HDR10 metadata".


## macOS

### 5. Verify pattern and HDR10 metadata

- Run on an HDR (EDR) display. Done when both swap chains show both patterns.
- Find out if macOS sends the codes unchanged to an external HDR10 display, or tone maps them.
