#pragma once

#include <cstdint>

#include "rust/cxx.h"

namespace nitpick {

// Shared types, defined by the generated swap_chain_probe.cxx.h.
enum class FrameFault : std::uint8_t;
struct MetadataCall;
struct SwapChainImage;
struct SwapChainRecord;

// Test probe of the pattern window (`pattern_window.cpp`). Call on the Qt GUI
// thread. All functions that need a window throw std::runtime_error if no
// pattern window is open.

// From the next open: create the swap chain with UsedAsTransferSource and
// read back the swap-chain image in each render that uploads a frame.
void setSwapChainReadback(bool enabled);
SwapChainRecord swapChainRecord();
// The readback of the last render that uploaded a frame, once.
SwapChainImage takeSwapChainImage();
// The next `count` calls of the step of `fault` give that fault.
void injectFrameFault(FrameFault fault, std::uint32_t count);
// While held, renders do nothing; a release requests an update.
void holdRenders(bool hold);
// Frame IDs since the last open: queued, and uploaded without an error.
rust::Vec<std::uint64_t> queuedFrames();
rust::Vec<std::uint64_t> uploadedFrames();
// The last native metadata call since the last open.
MetadataCall lastMetadataCall();
// Closes the window from its event loop, as Esc does.
void closePatternWindowLater();
// Runs the Qt event loop for `ms` milliseconds.
void runEventLoop(std::int32_t ms);

} // namespace nitpick
