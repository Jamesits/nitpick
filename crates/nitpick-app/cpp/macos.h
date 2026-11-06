#pragma once

#include <QtCore/QSize>

#include <cstdint>

class QScreen;
class QWindow;

namespace nitpick {

struct MetalHdrMetadata;

// Pixel size of a full-screen window on `screen`: the screen without the
// camera housing and the menu bar next to it.
QSize screenSafePixelSize(QScreen *screen);

// `CGDirectDisplayID` of `screen` (`NSScreenNumber`).
std::uint32_t displayIdOf(QScreen *screen);

// Sets the HDR10 metadata of the CAMetalLayer of `window`, or removes it if
// `metadata` is null.
void setLayerHdrMetadata(QWindow *window, const MetalHdrMetadata *metadata);

} // namespace nitpick
