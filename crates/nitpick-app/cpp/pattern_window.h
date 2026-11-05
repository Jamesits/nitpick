#pragma once

#include <cstdint>

#include "rust/cxx.h"

namespace nitpick {

// All functions throw std::runtime_error on failure; cxx maps it to Err.
std::int32_t screenPixelWidth(std::int32_t screen);
std::int32_t screenPixelHeight(std::int32_t screen);
rust::String showPattern(rust::Slice<const std::uint32_t> pixels, std::int32_t width,
                         std::int32_t height, std::int32_t screen);

} // namespace nitpick
