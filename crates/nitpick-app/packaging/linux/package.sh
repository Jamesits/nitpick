#!/bin/bash
# Self-contained Linux folder and tar.gz archive.
#
# Qt has no deploy tool for Linux outside CMake. linuxdeploy-plugin-qt also
# copies system libraries (glib, libstdc++, ...) that can conflict with the
# system graphics drivers, and it does not deploy the Wayland EGL client
# plugin that the launcher needs. This script copies only what the Qt
# installation supplies; Qt's CMake deployment also leaves out the system
# library directories. README.md lists the system requirements.
#
# The package keeps the directory layout of the Qt installation (lib/,
# plugins/, qml/), so the run paths in the Qt binaries stay valid.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
QMAKE="${QMAKE:-qmake}"
QT_LIBS=$("$QMAKE" -query QT_INSTALL_LIBS)
QT_PLUGINS=$("$QMAKE" -query QT_INSTALL_PLUGINS)
QT_QML=$("$QMAKE" -query QT_INSTALL_QML)
QT_LIBEXECS=$("$QMAKE" -query QT_INSTALL_LIBEXECS)
# Absolute, because the QML copy below runs in another directory.
TARGET=$(cd "$CARGO_MAKE_CRATE_TARGET_DIRECTORY" && pwd)
DIST="$TARGET/dist"
OUT="$DIST/nitpick"
VERSION=$(cargo pkgid -p nitpick-app | sed 's/.*[#@]//')
rm -rf "$OUT"
mkdir -p "$OUT/lib" "$OUT/plugins" "$OUT/qml"
cp "$TARGET/release/nitpick" "$OUT/nitpick"
# The debug information is 85 % of the executable. As with the PDB file on
# Windows, it stays in target/release; the symbols stay for the backtraces.
strip --strip-debug "$OUT/nitpick"

# Replace the build run path (the Qt installation). If it stays, a computer
# with the same Qt path loads that Qt instead of the packaged one, which hides
# missing files. The dynamic loader expands $ORIGIN, not the shell.
# shellcheck disable=SC2016
patchelf --set-rpath '$ORIGIN/lib' "$OUT/nitpick"
printf '[Paths]\nPrefix = .\n' >"$OUT/qt.conf"

# Platform plugins for X11 and Wayland. The launcher draws with OpenGL, so
# each platform also needs its EGL/GLX integration; the pattern window uses
# Vulkan, which needs no integration plugin.
mkdir -p "$OUT/plugins/platforms"
cp "$QT_PLUGINS/platforms/libqxcb.so" "$QT_PLUGINS/platforms/libqwayland.so" \
    "$OUT/plugins/platforms/"
for TYPE in xcbglintegrations wayland-shell-integration wayland-decoration-client \
    wayland-graphics-integration-client platforminputcontexts; do
    cp -r "$QT_PLUGINS/$TYPE" "$OUT/plugins/"
done
# The GTK 3 theme needs GTK on the system; the portal theme only needs D-Bus.
mkdir -p "$OUT/plugins/platformthemes"
cp "$QT_PLUGINS/platformthemes/libqxdgdesktopportal.so" "$OUT/plugins/platformthemes/"

# The QML is compiled into the executable; qmlimportscanner reads it only to
# find the imported modules. A module directory can contain other modules
# (QtQuick contains Dialogs, Particles, ...). Copy only the modules that the
# scanner reports, with their asset directories (no qmldir).
MODULES=$("$QT_LIBEXECS/qmlimportscanner" -rootPath "$HERE/../../qml" -importPath "$QT_QML" |
    python3 -c 'import json, sys; print("\n".join(m["path"] for m in json.load(sys.stdin) if "path" in m))')
while IFS= read -r MODULE; do
    REL="${MODULE#"$QT_QML"/}"
    (cd "$QT_QML" && find "$REL" -mindepth 1 -type d -exec test -e '{}/qmldir' \; -prune \
        -o -type f -exec cp --parents '{}' "$OUT/qml/" \;)
done <<<"$MODULES"

elfs() {
    echo "$OUT/nitpick"
    find "$OUT/plugins" "$OUT/qml" "$OUT/lib" -type f -name '*.so*'
}

# ldd lists the dependencies recursively, so one pass finds all libraries.
# LD_LIBRARY_PATH overrides the run paths, which point into the still empty
# package lib/ directory.
elfs | while read -r ELF; do LD_LIBRARY_PATH="$QT_LIBS" ldd "$ELF"; done |
    awk -v qt="$QT_LIBS/" '$2 == "=>" && index($3, qt) == 1 {print $3}' | sort -u |
    while read -r LIB; do cp -L "$LIB" "$OUT/lib/"; done

# Fail if a library is missing, or if a library that Qt supplies loads from
# outside the package (a system Qt or ICU of a different version).
PROBLEMS=$(elfs | while read -r ELF; do env -u LD_LIBRARY_PATH ldd "$ELF"; done |
    awk '$2 == "=>" {print $1, $3}' | sort -u |
    while read -r NAME LIB; do
        if [ "$LIB" = "not" ]; then
            echo "$NAME: not found"
        elif [ -e "$QT_LIBS/$NAME" ] && [ "${LIB#"$OUT"/}" = "$LIB" ]; then
            echo "$NAME: $LIB"
        fi
    done)
if [ -n "$PROBLEMS" ]; then
    echo "libraries missing from the package:" >&2
    echo "$PROBLEMS" >&2
    exit 1
fi

ARCHIVE="$DIST/nitpick-$VERSION-linux-$(uname -m).tar.gz"
tar -C "$DIST" --owner=0 --group=0 -czf "$ARCHIVE" nitpick
echo "package: $OUT"
echo "archive: $ARCHIVE"
