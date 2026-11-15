#pragma once

#include <QtCore/QSize>

#include <cstdint>
#include <string>

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

// Pixel format and colour space of the CAMetalLayer of `window`. `matches`
// is true for the layer that QRhi makes for the requested swap chain:
// RGBA16Float with extended linear sRGB if `fp16`, else RGB10A2Unorm with
// BT.2100 PQ.
struct MetalLayerFormat {
    std::string pixelFormat;
    std::string colourSpace;
    bool matches = false;
};
MetalLayerFormat metalLayerFormat(QWindow *window, bool fp16);

} // namespace nitpick
