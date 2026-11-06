# Nitpick

An opinionated color profiling, calibration and verification workstation.

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

Requirements: TBD

Development build: TBD

Packaging for release: TBD
