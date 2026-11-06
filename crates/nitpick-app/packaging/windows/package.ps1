# Self-contained Windows folder and zip archive (windeployqt).
$ErrorActionPreference = "Stop"

$QMAKE = if ($env:QMAKE) { $env:QMAKE } else { "qmake" }
$QT_BIN = & $QMAKE -query QT_INSTALL_BINS
$QML = Join-Path $PSScriptRoot "../../qml"
$TARGET = $env:CARGO_MAKE_CRATE_TARGET_DIRECTORY
$DIST = Join-Path $TARGET "dist"
$OUT = Join-Path $DIST "nitpick"
if (Test-Path $OUT) { Remove-Item -Recurse -Force $OUT }
New-Item -ItemType Directory $OUT | Out-Null
Copy-Item (Join-Path $TARGET "release/nitpick.exe") $OUT

# The QML is compiled into the executable; --qmldir only lets windeployqt find
# the imported modules. The launcher has no translations and QRhi uses
# Direct3D 11, so the Qt translations and the software OpenGL are not needed.
# windeployqt deploys the compiler runtime only as the vc_redist installer, so
# the runtime DLLs are copied below instead (app-local deployment).
& (Join-Path $QT_BIN "windeployqt.exe") --release --no-translations --no-opengl-sw `
    --no-compiler-runtime --qmldir $QML (Join-Path $OUT "nitpick.exe")
if ($LASTEXITCODE) { exit $LASTEXITCODE }

$VSWHERE = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$CRT = & $VSWHERE -latest -products * -find "VC\Redist\MSVC\*\x64\Microsoft.VC*.CRT" |
    Sort-Object | Select-Object -Last 1
if (-not $CRT) { throw "MSVC runtime (Microsoft.VC*.CRT) not found" }
Copy-Item (Join-Path $CRT "*.dll") $OUT

$VERSION = (cargo pkgid -p nitpick-app) -replace '.*[#@]', ''
$ARCHIVE = Join-Path $DIST "nitpick-$VERSION-windows-x64.zip"
Compress-Archive -Path $OUT -DestinationPath $ARCHIVE -Force
Write-Output "package: $OUT" "archive: $ARCHIVE"
