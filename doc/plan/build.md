# Plan: Build System

## macOS

### 4. Build and run on macOS

Run the launcher with Homebrew Qt 6.12. Verify on an HDR display:
- The HDR10 pattern shows without an error, also with HDR10 metadata.
- The pattern fills the safe area and the status line has no size warning.
- The reference table size equals the pattern size in the status line.

## Linux

### 6. NVIDIA EGL on Fedora 44

The launcher does not show on Fedora 44 with the NVIDIA driver. The clean install test found this in a container. The container used the host NVIDIA 595 driver (NVIDIA Container Toolkit, `--gpus all`, `NVIDIA_DRIVER_CAPABILITIES=all`), the host GNOME 50 Wayland socket and `/dev/dri`:
- Qt makes an OpenGL ES 3.2 context on the NVIDIA GPU, but the window surface fails: `Could not create EGL surface (EGL error 0x3003)` (`EGL_BAD_ALLOC`), then `eglSwapBuffers failed with 0x300d` (`EGL_BAD_SURFACE`).
- Fedora's own `es2gears_wayland` (`mesa-demos`) fails in the same container: `EGLUT: failed to create surface`.
- With Mesa only (no NVIDIA driver), the launcher and `es2gears_wayland` work.
- The HDR10 pattern (Vulkan) opens with the NVIDIA driver.
- Ubuntu 24.04 and 26.04 containers with the same driver work.

Open question: is the cause the container setup or the package? **Unverified:** the toolkit mounts the host (Ubuntu) driver files in `/usr/lib/x86_64-linux-gnu`, which is not a Fedora library directory. Because `es2gears_wayland` also fails, the container setup is the likely cause.

TODO:
- Run the package on a Fedora 44 installation with the NVIDIA driver from RPM Fusion (`akmod-nvidia`, which installs `egl-wayland`). Use bare metal or a VM with GPU passthrough.
- Verify: with `QSG_INFO=1`, the launcher log shows the NVIDIA renderer and no EGL surface error. The launcher shows, and the HDR10 pattern opens with and without HDR10 metadata.
- If it works: remove this item.
- If it fails: compare with a Qt application from the Fedora repositories. If that one works, compare its Wayland EGL client plugin (`wayland-graphics-integration-client/libqt-plugin-wayland-egl.so`) and its EGL libraries with the package.
