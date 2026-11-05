#pragma once

class QWindow;

namespace nitpick {

struct HdrMetadata;

// Sets the HDR10 metadata of the CAMetalLayer of `window`.
void setMetalHdrMetadata(QWindow *window, const HdrMetadata &metadata);

} // namespace nitpick
