#!/bin/bash
# Self-contained macOS app bundle and zip archive (macdeployqt).
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
QMAKE="${QMAKE:-qmake}"
QT_BIN=$("$QMAKE" -query QT_INSTALL_BINS)
QT_LIBS=$("$QMAKE" -query QT_INSTALL_LIBS)
TARGET="$CARGO_MAKE_CRATE_TARGET_DIRECTORY"
DIST="$TARGET/dist"
APP="$DIST/nitpick.app"
VERSION=$(cargo pkgid -p nitpick-app | sed 's/.*[#@]//')
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$TARGET/release/nitpick" "$APP/Contents/MacOS/nitpick"

# Remove the build rpath points at the Qt installation. If it stays, a Mac with the
# same Qt loads that Qt instead of the bundled one, which hides missing files.
EXE="$APP/Contents/MacOS/nitpick"
for RPATH in $(otool -l "$EXE" | awk '/cmd LC_RPATH/ {getline; getline; print $2}'); do
    case "$RPATH" in @*) ;; *) install_name_tool -delete_rpath "$RPATH" "$EXE" ;; esac
done

PLIST="$APP/Contents/Info.plist"
cp "$HERE/Info.plist" "$PLIST"
plutil -replace CFBundleShortVersionString -string "$VERSION" "$PLIST"
plutil -replace CFBundleVersion -string "$VERSION" "$PLIST"

# The QML is compiled into the executable; -qmldir only lets macdeployqt find
# the imported modules. Homebrew installs each Qt module in its own prefix;
# -libpath adds the directory that links them all. Signing comes last because
# Info.plist still changes.
"$QT_BIN/macdeployqt" "$APP" -qmldir="$HERE/../../qml" -libpath="$QT_LIBS" -no-codesign

deps() {
    # skips the install name of a library itself
    ID=$(otool -D "$1" 2>/dev/null | tail -n +2)
    otool -L "$1" 2>/dev/null | tail -n +2 | awk '{print $1}' | grep -vxF "${ID:-//}" || true
}
export -f deps
# All @rpath entries of the bundle resolve to Contents/Frameworks.
FRAMEWORKS="$APP/Contents/Frameworks"

# macdeployqt copies whole QML module trees (QtQuick includes VirtualKeyboard,
# Scene3D, Pdf, ...) and every plugin of a plugin type, also plugins whose Qt
# modules it does not deploy. Those plugins cannot load; remove them and the
# QML links to them.
while IFS= read -r -d '' PLUGIN; do
    for DEP in $(deps "$PLUGIN" | grep '^@rpath/'); do
        if [ ! -e "$FRAMEWORKS/${DEP#@rpath/}" ]; then
            rm "$PLUGIN"
            break
        fi
    done
done < <(find "$APP/Contents/PlugIns" -name '*.dylib' -print0)
find "$APP/Contents/Resources/qml" -type l ! -exec test -e {} \; -delete

# The minimum macOS is the highest deployment target of all bundled binaries,
# rounded down to the major version. Homebrew bottles target a major version,
# but carry the minor version of the build machine (harfbuzz: 27.2, runs on
# 27.0.1). Finder refuses to open a bundle with a newer LSMinimumSystemVersion;
# dyld does not check the libraries.
MINOS=$(find "$APP" -type f -exec sh -c 'vtool -show-build "$1" 2>/dev/null' _ {} \; |
    awk '$1 == "minos" {print $2}' | sort -V | tail -1)
MINOS="${MINOS%%.*}.0"
[ "$MINOS" != ".0" ] || { echo "no deployment target found" >&2; exit 1; }
plutil -insert LSMinimumSystemVersion -string "$MINOS" "$PLIST"

# Fail if a bundled binary still loads a library from outside the bundle and
# the system, or a framework that is not bundled.
LEAKS=$(find "$APP" -type f -exec bash -c 'deps "$1"' _ {} \; | sort -u |
    while read -r DEP; do
        case "$DEP" in
        (@rpath/*) [ -e "$FRAMEWORKS/${DEP#@rpath/}" ] || echo "$DEP" ;;
        (@* | /System/* | /usr/lib/*) ;;
        (*) echo "$DEP" ;;
        esac
    done)
if [ -n "$LEAKS" ]; then
    echo "libraries missing from the bundle:" >&2
    echo "$LEAKS" >&2
    exit 1
fi

codesign --force --deep --sign - "$APP"

ARCHIVE="$DIST/nitpick-$VERSION-macos-$(uname -m).zip"
rm -f "$ARCHIVE"
# ditto keeps the framework symlinks.
ditto -c -k --sequesterRsrc --keepParent "$APP" "$ARCHIVE"
echo "package: $APP"
echo "archive: $ARCHIVE"
