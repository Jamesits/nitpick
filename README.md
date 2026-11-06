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

Requirements:

- GCC or Clang
- Vulkan headers (`libvulkan-dev` on Debian and Ubuntu)
- Qt 6.12 with private headers and Qt Shader Tools. The binary must use the Qt of its headers. If the distribution has a different Qt version, install Qt with [aqtinstall](https://github.com/miurahr/aqtinstall):

```sh
uvx --from aqtinstall aqt install-qt linux desktop 6.12.0 linux_gcc_64 -m qtshadertools -O ~/Qt
```

Development build:

```sh
export QMAKE=~/Qt/6.12.0/gcc_64/bin/qmake
cargo run -p nitpick-app
```

The binary finds the Qt libraries through its run path. The build warning "Neither mold, lld, nor gold linkers were found" has no effect: Rust links with its own `rust-lld`.

Packaging for release: TBD
