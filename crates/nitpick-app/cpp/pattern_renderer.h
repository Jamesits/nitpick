#pragma once

#include <QtCore/QByteArray>
#include <QtCore/QPoint>
#include <QtCore/QSize>
#include <rhi/qrhi.h>

#include <memory>

#if QT_CONFIG(vulkan) && __has_include(<vulkan/vulkan.h>)
#define NITPICK_HAS_VULKAN 1
#include <QtGui/QVulkanInstance>
#endif

namespace nitpick {

// Format of the pattern texture and of a render target that stores it
// unchanged: RGB10A2 for the HDR10 swap chain, RGBA16F for FP16 scRGB.
QRhiTexture::Format patternTextureFormat(bool fp16);

// The draw path of the pattern: texture upload, copy shader and pipeline.
// The pattern window draws it into the swap chain; the readback check
// draws it into an offscreen texture.
class PatternRenderer {
public:
    // `rp` must be compatible with every target of draw().
    PatternRenderer(QRhi *rhi, QRhiRenderPassDescriptor *rp, QRhiTexture::Format format);
    ~PatternRenderer();

    PatternRenderer(const PatternRenderer &) = delete;
    PatternRenderer &operator=(const PatternRenderer &) = delete;

    // Builds the texture, bindings and pipeline for an image of `size` and
    // queues the upload of `pixels` (tightly packed rows) into `updates`.
    // Throws std::runtime_error. Builds new objects first, so that a
    // failure keeps the previous pattern.
    void upload(QRhiResourceUpdateBatch *updates, const QByteArray &pixels, QSize size);

    // One pass over `target`: the last uploaded pattern 1:1 from `origin`,
    // opaque black elsewhere. `after` is applied at the end of the pass, for
    // example a readback.
    void draw(QRhiCommandBuffer *cb, QRhiRenderTarget *target, QPoint origin,
              QRhiResourceUpdateBatch *updates, QRhiResourceUpdateBatch *after = nullptr);

private:
    QRhi *m_rhi;
    QRhiRenderPassDescriptor *m_rp;
    QRhiTexture::Format m_format;
    std::unique_ptr<QRhiSampler> m_sampler;
    std::unique_ptr<QRhiBuffer> m_area;
    std::unique_ptr<QRhiTexture> m_tex;
    std::unique_ptr<QRhiShaderResourceBindings> m_srb;
    std::unique_ptr<QRhiGraphicsPipeline> m_ps;
};

#if defined(NITPICK_HAS_VULKAN)
// Created once and never destroyed: windows and devices that use it may
// live until the application exits. Needs a QGuiApplication. Not named
// `vulkanInstance`: inside a QWindow subclass that name finds
// QWindow::vulkanInstance(), which returns null here.
QVulkanInstance *sharedVulkanInstance();
#endif

} // namespace nitpick
