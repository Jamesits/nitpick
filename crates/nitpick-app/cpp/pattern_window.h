#pragma once

#include <cstdint>

#include "rust/cxx.h"

namespace nitpick {

// Shared structs, defined by the generated display.cxx.h.
struct HdrMetadata;
struct PatternSurface;
struct ScreenIdentity;

// All functions throw std::runtime_error on failure; cxx maps it to Err.
std::int32_t screenPixelWidth(std::int32_t screen);
std::int32_t screenPixelHeight(std::int32_t screen);
ScreenIdentity screenIdentity(std::int32_t screen);
PatternSurface openPatternWindow(std::int32_t screen, bool fp16);
rust::String presentPattern(rust::Slice<const std::uint8_t> pixels, std::int32_t width,
                            std::int32_t height, const HdrMetadata &metadata);

} // namespace nitpick
