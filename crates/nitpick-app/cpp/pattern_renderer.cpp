#include "nitpick-app/cpp/pattern_renderer.h"

#include <QtGui/QColor>
#include <rhi/qrhi_platform.h>

#include <cstdint>
#include <stdexcept>

#include "pattern_shaders.h"

namespace nitpick {

namespace {

QShader loadShader(const unsigned char *data, std::size_t size) {
    QShader s = QShader::fromSerialized(
        QByteArray::fromRawData(reinterpret_cast<const char *>(data), qsizetype(size)));
    if (!s.isValid()) {
        throw std::runtime_error("embedded shader is not valid");
    }
    return s;
}

} // namespace

QRhiTexture::Format patternTextureFormat(bool fp16) {
    return fp16 ? QRhiTexture::RGBA16F : QRhiTexture::RGB10A2;
}

PatternRenderer::PatternRenderer(QRhi *rhi, QRhiRenderPassDescriptor *rp,
                                 QRhiTexture::Format format)
    : m_rhi(rhi), m_rp(rp), m_format(format) {
    m_sampler.reset(m_rhi->newSampler(QRhiSampler::Nearest, QRhiSampler::Nearest,
                                      QRhiSampler::None, QRhiSampler::ClampToEdge,
                                      QRhiSampler::ClampToEdge));
    if (!m_sampler->create()) {
        throw std::runtime_error("cannot create the sampler");
    }
    // std140 `ivec2 origin`, padded to 16 bytes.
    m_area.reset(m_rhi->newBuffer(QRhiBuffer::Dynamic, QRhiBuffer::UniformBuffer, 16));
    if (!m_area->create()) {
        throw std::runtime_error("cannot create the pattern area buffer");
    }
}

PatternRenderer::~PatternRenderer() = default;

void PatternRenderer::upload(QRhiResourceUpdateBatch *updates, const QByteArray &pixels,
                             QSize size) {
    std::unique_ptr<QRhiTexture> tex(m_rhi->newTexture(m_format, size));
    if (!tex->create()) {
        throw std::runtime_error("cannot create the pattern texture");
    }
    std::unique_ptr<QRhiShaderResourceBindings> srb(m_rhi->newShaderResourceBindings());
    srb->setBindings(
        {QRhiShaderResourceBinding::uniformBuffer(0, QRhiShaderResourceBinding::FragmentStage,
                                                  m_area.get()),
         QRhiShaderResourceBinding::sampledTexture(1, QRhiShaderResourceBinding::FragmentStage,
                                                   tex.get(), m_sampler.get())});
    if (!srb->create()) {
        throw std::runtime_error("cannot create the shader resource bindings");
    }
    std::unique_ptr<QRhiGraphicsPipeline> ps(m_rhi->newGraphicsPipeline());
    ps->setShaderStages(
        {{QRhiShaderStage::Vertex, loadShader(kPatternVertQsb, sizeof kPatternVertQsb)},
         {QRhiShaderStage::Fragment, loadShader(kPatternFragQsb, sizeof kPatternFragQsb)}});
    ps->setVertexInputLayout({});
    ps->setShaderResourceBindings(srb.get());
    ps->setRenderPassDescriptor(m_rp);
    if (!ps->create()) {
        throw std::runtime_error("cannot create the graphics pipeline");
    }
    updates->uploadTexture(
        tex.get(), QRhiTextureUploadEntry(0, 0, QRhiTextureSubresourceUploadDescription(pixels)));
    m_ps = std::move(ps);
    m_srb = std::move(srb);
    m_tex = std::move(tex);
}

void PatternRenderer::draw(QRhiCommandBuffer *cb, QRhiRenderTarget *target, QPoint origin,
                           QRhiResourceUpdateBatch *updates, QRhiResourceUpdateBatch *after) {
    const std::int32_t area[2] = {origin.x(), origin.y()};
    updates->updateDynamicBuffer(m_area.get(), 0, sizeof area, area);
    cb->beginPass(target, Qt::black, {1.0f, 0}, updates);
    if (m_ps) {
        const QSize s = target->pixelSize();
        cb->setGraphicsPipeline(m_ps.get());
        cb->setViewport({0, 0, float(s.width()), float(s.height())});
        cb->setShaderResources();
        cb->draw(3);
    }
    cb->endPass(after);
}

#if defined(NITPICK_HAS_VULKAN)
QVulkanInstance *sharedVulkanInstance() {
    static QVulkanInstance *instance = [] {
        auto *inst = new QVulkanInstance;
        // Includes VK_EXT_swapchain_colorspace, which HDR swap chains need.
        inst->setExtensions(QRhiVulkanInitParams::preferredInstanceExtensions());
        if (!inst->create()) {
            delete inst;
            throw std::runtime_error("cannot create a Vulkan instance");
        }
        return inst;
    }();
    return instance;
}
#endif

} // namespace nitpick
