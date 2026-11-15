#pragma once

#include <cstdint>
#include <memory>

#include "rust/cxx.h"

namespace nitpick {

// Shared types, defined by the generated readback.cxx.h.
enum class OffscreenBackend : std::uint8_t;
struct OffscreenFrame;
struct OffscreenReadback;

// A QRhi device without a window that draws the pattern with
// PatternRenderer into an offscreen texture of the swap chain format and
// reads the texture back. Use it on one thread.
class OffscreenRenderer {
public:
    struct Device;

    explicit OffscreenRenderer(std::unique_ptr<Device> device);
    ~OffscreenRenderer();

    rust::String backendName() const;
    rust::String deviceName() const;
    // QRhiDriverInfo::CpuDevice: for example WARP or lavapipe.
    bool isSoftwareDevice() const;

    // One offscreen frame: upload, draw and readback. Throws
    // std::runtime_error.
    OffscreenReadback draw(const OffscreenFrame &frame, rust::Slice<const std::uint8_t> pixels);

private:
    std::unique_ptr<Device> m_device;
};

// Throws std::runtime_error if the backend is not built in or no device
// is created. Vulkan needs a QGuiApplication. `software` sets
// QRhi::PreferSoftwareRenderer; QRhi ignores it where no software device
// exists, so check isSoftwareDevice().
std::unique_ptr<OffscreenRenderer> createOffscreenRenderer(OffscreenBackend backend, bool software);

} // namespace nitpick
