#include "nitpick-app/cpp/readback.h"
#include "nitpick-app/cpp/pattern_renderer.h"

#include "nitpick-app/src/readback.cxx.h"

#include <QtGui/QGuiApplication>
#include <rhi/qrhi.h>
#include <rhi/qrhi_platform.h>

#include <stdexcept>
#include <string>

namespace nitpick {

struct OffscreenRenderer::Device {
    std::unique_ptr<QRhi> rhi;
};

namespace {

std::unique_ptr<QRhi> createRhi(OffscreenBackend backend, QRhi::Flags flags) {
    switch (backend) {
    case OffscreenBackend::D3d11: {
#if defined(Q_OS_WIN)
        QRhiD3D11InitParams params;
        return std::unique_ptr<QRhi>(QRhi::create(QRhi::D3D11, &params, flags));
#else
        throw std::runtime_error("D3D11 needs Windows");
#endif
    }
    case OffscreenBackend::Vulkan: {
#if defined(NITPICK_HAS_VULKAN)
        if (!QGuiApplication::instance()) {
            throw std::runtime_error("the Vulkan instance needs a QGuiApplication");
        }
        QRhiVulkanInitParams params;
        params.inst = sharedVulkanInstance();
        return std::unique_ptr<QRhi>(QRhi::create(QRhi::Vulkan, &params, flags));
#else
        throw std::runtime_error(
            "built without Vulkan: Qt has no Vulkan support or the build found no "
            "vulkan/vulkan.h (set VULKAN_SDK)");
#endif
    }
    case OffscreenBackend::Metal: {
#if defined(Q_OS_MACOS)
        QRhiMetalInitParams params;
        return std::unique_ptr<QRhi>(QRhi::create(QRhi::Metal, &params, flags));
#else
        throw std::runtime_error("Metal needs macOS");
#endif
    }
    }
    throw std::runtime_error("unknown QRhi backend");
}

std::string frameOpError(const char *call, QRhi::FrameOpResult r) {
    switch (r) {
    case QRhi::FrameOpDeviceLost:
        return std::string(call) + " failed: graphics device lost";
    default:
        return std::string(call) + " failed";
    }
}

} // namespace

OffscreenRenderer::OffscreenRenderer(std::unique_ptr<Device> device)
    : m_device(std::move(device)) {}

OffscreenRenderer::~OffscreenRenderer() = default;

rust::String OffscreenRenderer::backendName() const {
    return rust::String(m_device->rhi->backendName());
}

rust::String OffscreenRenderer::deviceName() const {
    return rust::String(m_device->rhi->driverInfo().deviceName.toStdString());
}

bool OffscreenRenderer::isSoftwareDevice() const {
    return m_device->rhi->driverInfo().deviceType == QRhiDriverInfo::CpuDevice;
}

OffscreenReadback OffscreenRenderer::draw(const OffscreenFrame &frame,
                                          rust::Slice<const std::uint8_t> pixels) {
    QRhi *rhi = m_device->rhi.get();
    const QRhiTexture::Format format = patternTextureFormat(frame.fp16);
    const QSize image(frame.width, frame.height);
    const QSize targetSize(frame.target_width, frame.target_height);
    // The swap chain formats: HDR10 is R10G10B10A2 unorm, FP16 scRGB is
    // RGBA16F. A target of this format stores the shader output like the
    // swap chain back buffer.
    const std::size_t bytesPerPixel = frame.fp16 ? 8 : 4;
    if (frame.width <= 0 || frame.height <= 0 ||
        pixels.size() != std::size_t(frame.width) * std::size_t(frame.height) * bytesPerPixel) {
        throw std::runtime_error("pixel buffer does not match the image size");
    }
    const QRhiTexture::Flags targetFlags =
        QRhiTexture::RenderTarget | QRhiTexture::UsedAsTransferSource;
    if (!rhi->isTextureFormatSupported(format) ||
        !rhi->isTextureFormatSupported(format, targetFlags)) {
        throw std::runtime_error("the device does not support the pattern texture format");
    }
    std::unique_ptr<QRhiTexture> target(rhi->newTexture(format, targetSize, 1, targetFlags));
    if (!target->create()) {
        throw std::runtime_error("cannot create the offscreen target");
    }
    // Declared before the render target, so destroyed after it.
    std::unique_ptr<QRhiRenderPassDescriptor> rp;
    std::unique_ptr<QRhiTextureRenderTarget> rt(rhi->newTextureRenderTarget({target.get()}));
    rp.reset(rt->newCompatibleRenderPassDescriptor());
    rt->setRenderPassDescriptor(rp.get());
    if (!rt->create()) {
        throw std::runtime_error("cannot create the offscreen render target");
    }
    // Declared last, so destroyed first.
    PatternRenderer renderer(rhi, rp.get(), format);

    QRhiResourceUpdateBatch *updates = rhi->nextResourceUpdateBatch();
    try {
        renderer.upload(updates,
                        QByteArray(reinterpret_cast<const char *>(pixels.data()),
                                   qsizetype(pixels.size())),
                        image);
    } catch (...) {
        updates->release();
        throw;
    }
    QRhiCommandBuffer *cb = nullptr;
    QRhi::FrameOpResult r = rhi->beginOffscreenFrame(&cb);
    if (r != QRhi::FrameOpSuccess) {
        updates->release();
        throw std::runtime_error(frameOpError("beginOffscreenFrame", r));
    }
    QRhiReadbackResult result;
    QRhiResourceUpdateBatch *readback = rhi->nextResourceUpdateBatch();
    readback->readBackTexture(QRhiReadbackDescription(target.get()), &result);
    renderer.draw(cb, rt.get(), QPoint(frame.origin_x, frame.origin_y), updates, readback);
    // Offscreen frames wait for the GPU: the readback is complete on return.
    r = rhi->endOffscreenFrame();
    if (r != QRhi::FrameOpSuccess) {
        throw std::runtime_error(frameOpError("endOffscreenFrame", r));
    }
    if (result.format != format || result.pixelSize != targetSize) {
        throw std::runtime_error("the readback has another format or size than the target");
    }
    OffscreenReadback out{};
    out.data.reserve(std::size_t(result.data.size()));
    for (const char byte : result.data) {
        out.data.push_back(static_cast<std::uint8_t>(byte));
    }
    out.width = result.pixelSize.width();
    out.height = result.pixelSize.height();
    out.y_up = rhi->isYUpInFramebuffer();
    return out;
}

std::unique_ptr<OffscreenRenderer> createOffscreenRenderer(OffscreenBackend backend,
                                                           bool software) {
    QRhi::Flags flags;
    if (software) {
        flags |= QRhi::PreferSoftwareRenderer;
    }
    auto device = std::make_unique<OffscreenRenderer::Device>();
    device->rhi = createRhi(backend, flags);
    if (!device->rhi) {
        throw std::runtime_error("cannot create the QRhi device");
    }
    return std::make_unique<OffscreenRenderer>(std::move(device));
}

} // namespace nitpick
