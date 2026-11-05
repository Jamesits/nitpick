# Nitpick

An opinionated color profiling, calibration and verification workstation.

## Building

### Windows

Requirements:

- Visual Studio C++ build tools

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

Requirements: TBD

Development build: TBD

Packaging for release: TBD

### Linux

Requirements: TBD

Development build: TBD

Packaging for release: TBD
