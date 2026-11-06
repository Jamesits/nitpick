# Nitpick

An opinionated color profiling, calibration and verification workstation.

![Project Status - Development](https://img.shields.io/badge/Project_Status-Development-red) ![100% Written by AI](https://img.shields.io/badge/Written_by_AI-100%25-blue)

## Building

### Windows

Requirements:

- Visual Studio C++ build tools
- Qt

Development build:

```powershell
$env:QMAKE="X:\path\to\Qt\6.12.0\msvc2022_64\bin\qmake.exe"
$env:PATH="X:\path\to\Qt\6.12.0\msvc2022_64\bin:$env:PATH"
cargo run -p nitpick-app
```

Packaging for release:

```powershell
$env:QMAKE="X:\path\to\Qt\6.12.0\msvc2022_64\bin\qmake.exe"
cargo install cargo-make
cargo make package
```

### macOS

Requirements:

- Xcode
- Qt (`brew install qt`)

Development build:

```sh
export QMAKE=/opt/homebrew/bin/qmake
cargo run -p nitpick-app
```

Packaging for release:

```sh
export QMAKE=/opt/homebrew/bin/qmake
cargo install cargo-make
cargo make package
```

### Linux

<details>
<summary>Requirements</summary>

- GCC or Clang
- Vulkan headers (`libvulkan-dev` on Debian and Ubuntu)
- Qt 6.12 with private headers and Qt Shader Tools. The binary must use the Qt of its headers. If the distribution has a different Qt version, install Qt with [aqtinstall](https://github.com/miurahr/aqtinstall):

```sh
uvx --from aqtinstall aqt install-qt linux desktop 6.12.0 linux_gcc_64 -m qtshadertools -O ~/Qt
```

</details>

Development build:

```sh
export QMAKE=~/Qt/6.12.0/gcc_64/bin/qmake
cargo run -p nitpick-app
```

Notes:

- The binary finds the Qt libraries through its run path
- The build warning "Neither mold, lld, nor gold linkers were found" can be safely ignored, Rust links with its own `rust-lld`

<details>
<summary>Packaging for release</summary>

Build requirements (in addition to the above ones):

- `patchelf`
- Python 3

Run:

```sh
export QMAKE=~/Qt/6.12.0/gcc_64/bin/qmake
cargo install cargo-make
cargo make package
```

Runtime dependencies:

- glibc >=2.34
- libstdc++
- An X11 or Wayland session, an OpenGL (EGL or GLX) driver and a Vulkan driver
- Desktop libraries

```sh
# Debian/Ubuntu
sudo apt install libdbus-1-3 libegl1 libfontconfig1 libfreetype6 libgl1 libglib2.0-0t64 libgssapi-krb5-2 \
  libwayland-client0 libwayland-cursor0 libwayland-egl1 libx11-6 libx11-xcb1 libxcb1 libxcb-cursor0 \
  libxcb-glx0 libxcb-icccm4 libxcb-image0 libxcb-keysyms1 libxcb-randr0 libxcb-render0 libxcb-render-util0 \
  libxcb-shape0 libxcb-shm0 libxcb-sync1 libxcb-util1 libxcb-xfixes0 libxcb-xkb1 libxkbcommon0 \
  libxkbcommon-x11-0 libvulkan1 fonts-dejavu-core
```
</details>
