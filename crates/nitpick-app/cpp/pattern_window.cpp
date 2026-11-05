#include "nitpick-app/cpp/pattern_window.h"

#include "nitpick-app/src/display.cxx.h"

#include <QtCore/QEventLoop>
#include <QtCore/QPointer>
#include <QtCore/QTimer>
#include <QtGui/QGuiApplication>
#include <QtGui/QKeyEvent>
#include <QtGui/QPlatformSurfaceEvent>
#include <QtGui/QScreen>
#include <QtGui/QWindow>
#include <rhi/qrhi.h>
#include <rhi/qrhi_platform.h>

#include <cmath>
#include <cstdio>
#include <functional>
#include <memory>
#include <stdexcept>
#include <string>

#include "pattern_shaders.h"

#if defined(Q_OS_WIN)
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>

#include <QtGui/private/qrhid3d11_p.h>
#include <QtGui/qscreen_platform.h>
#include <dxgi1_6.h>
#include <wrl/client.h>
using Microsoft::WRL::ComPtr;
#elif defined(Q_OS_MACOS)
#include "nitpick-app/cpp/metal_hdr.h"
#else
#include <QtGui/QVulkanInstance>
#include <QtGui/private/qrhivulkan_p.h>
#endif

namespace {

using nitpick::HdrMetadata;

// Time for the compositor to map and expose the full-screen window.
constexpr int kInitTimeoutMs = 5000;

template <typename... Args> std::string sformat(const char *fmt, Args... args) {
    char buf[512];
    std::snprintf(buf, sizeof buf, fmt, args...);
    return buf;
}

QScreen *screenAt(std::int32_t index) {
    const auto screens = QGuiApplication::screens();
    if (index < 0 || index >= screens.size()) {
        throw std::runtime_error("no screen with index " + std::to_string(index));
    }
    return screens.at(index);
}

QShader loadShader(const unsigned char *data, std::size_t size) {
    QShader s = QShader::fromSerialized(
        QByteArray::fromRawData(reinterpret_cast<const char *>(data), qsizetype(size)));
    if (!s.isValid()) {
        throw std::runtime_error("embedded shader is not valid");
    }
    return s;
}

#if defined(Q_OS_WIN)

void check(HRESULT hr, const char *what) {
    if (FAILED(hr)) {
        throw std::runtime_error(
            sformat("%s failed: 0x%08lX", what, static_cast<unsigned long>(hr)));
    }
}

HMONITOR monitorOf(QScreen *screen) {
    auto *native = screen->nativeInterface<QNativeInterface::QWindowsScreen>();
    if (!native) {
        throw std::runtime_error("screen has no Windows native interface");
    }
    return native->handle();
}

// Physical pixel rectangle of the monitor. Qt makes the process per-monitor
// DPI aware, so this is the native mode, not the scaled desktop size.
RECT monitorRect(QScreen *screen) {
    MONITORINFO info{};
    info.cbSize = sizeof info;
    if (!GetMonitorInfoW(monitorOf(screen), &info)) {
        throw std::runtime_error("GetMonitorInfoW failed");
    }
    return info.rcMonitor;
}

// Finds the adapter that scans out `monitor`. Rendering on that adapter
// avoids a cross-adapter copy on hybrid-GPU systems.
void findOutput(HMONITOR monitor, LUID &adapterOut, ComPtr<IDXGIOutput6> &outputOut) {
    ComPtr<IDXGIFactory1> factory;
    check(CreateDXGIFactory1(IID_PPV_ARGS(&factory)), "CreateDXGIFactory1");
    ComPtr<IDXGIAdapter1> adapter;
    for (UINT a = 0; factory->EnumAdapters1(a, &adapter) != DXGI_ERROR_NOT_FOUND; ++a) {
        ComPtr<IDXGIOutput> output;
        for (UINT o = 0; adapter->EnumOutputs(o, &output) != DXGI_ERROR_NOT_FOUND; ++o) {
            DXGI_OUTPUT_DESC desc{};
            if (SUCCEEDED(output->GetDesc(&desc)) && desc.Monitor == monitor &&
                SUCCEEDED(output.As(&outputOut))) {
                DXGI_ADAPTER_DESC1 ad{};
                check(adapter->GetDesc1(&ad), "GetDesc1");
                adapterOut = ad.AdapterLuid;
                return;
            }
        }
    }
    throw std::runtime_error("no DXGI output for the selected screen");
}

std::string describeOutput(IDXGIOutput6 *output) {
    DXGI_OUTPUT_DESC1 d{};
    if (FAILED(output->GetDesc1(&d))) {
        return "output description unavailable";
    }
    const bool hdr = d.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
    char name[64];
    std::snprintf(name, sizeof name, "%ls", d.DeviceName);
    return sformat("%s: %u bpc, Windows HDR %s, EDID peak %.0f cd/m2, min %.4f cd/m2", name,
                  d.BitsPerColor, hdr ? "on" : "OFF (signal is converted to SDR)",
                  d.MaxLuminance, d.MinLuminance);
}

#elif !defined(Q_OS_MACOS)

// Created once and never destroyed: windows that use it may live until the
// application exits.
QVulkanInstance *vulkanInstance() {
    static QVulkanInstance *instance = [] {
        auto *inst = new QVulkanInstance;
        // Includes VK_EXT_swapchain_colorspace, which HDR swap chains need.
        inst->setExtensions(QRhiVulkanInitParams::preferredInstanceExtensions());
        if (!inst->create()) {
            throw std::runtime_error("cannot create a Vulkan instance");
        }
        return inst;
    }();
    return instance;
}

#endif

// Borderless full-screen window that scans out one static image through
// QRhi: D3D11 on Windows, Metal on macOS, Vulkan elsewhere.
//
// The swap chain is HDR10 (10-bit unorm, PQ, BT.2020) or FP16 scRGB. The
// fragment shader copies one texel per pixel with texelFetch: no sampling,
// blending or colour conversion inside the application. The compositor and
// the driver may still alter values after the present, which is what the
// external scope verifies.
class PatternWindow final : public QWindow {
public:
    PatternWindow(QScreen *screen, bool fp16) : m_fp16(fp16) {
#if defined(Q_OS_WIN)
        setSurfaceType(QSurface::Direct3DSurface);
        findOutput(monitorOf(screen), m_adapter, m_output);
#elif defined(Q_OS_MACOS)
        setSurfaceType(QSurface::MetalSurface);
#else
        setSurfaceType(QSurface::VulkanSurface);
        setVulkanInstance(vulkanInstance());
#endif
        setFlags(Qt::Window | Qt::FramelessWindowHint);
        setScreen(screen);
        setGeometry(screen->geometry());
        setCursor(Qt::BlankCursor);
        setTitle(QStringLiteral("nitpick pattern"));
    }

    // Blocks until the first expose has created the swap chain.
    void waitUntilReady() {
        if (!m_ready && m_error.empty()) {
            QEventLoop loop;
            m_onInit = [&loop] { loop.quit(); };
            QTimer::singleShot(kInitTimeoutMs, &loop, &QEventLoop::quit);
            loop.exec(QEventLoop::ExcludeUserInputEvents);
            m_onInit = nullptr;
        }
        if (!m_error.empty()) {
            throw std::runtime_error(m_error);
        }
        if (!m_ready) {
            throw std::runtime_error("the pattern window was not exposed in time");
        }
    }

    QSize pixelSize() const { return m_sc->currentPixelSize(); }

    std::string describe() const {
        std::string s = m_fp16 ? "FP16 scRGB swap chain" : "HDR10 swap chain";
        s += " (" + std::string(m_rhi->backendName()) + ")";
#if defined(Q_OS_WIN)
        s += "; " + describeOutput(m_output.Get());
        const RECT r = monitorRect(screen());
        if (r.right - r.left != pixelSize().width() || r.bottom - r.top != pixelSize().height()) {
            s += "; WARNING: swap chain size differs from the native screen size";
        }
#else
        const QRhiSwapChainHdrInfo i = m_sc->hdrInfo();
        if (i.limitsType == QRhiSwapChainHdrInfo::LuminanceInNits) {
            s += sformat("; display %.4f to %.0f cd/m2", i.limits.luminanceInNits.minLuminance,
                        i.limits.luminanceInNits.maxLuminance);
        } else {
            s += sformat("; EDR headroom %.2f (potential %.2f)",
                        i.limits.colorComponentValue.maxColorComponentValue,
                        i.limits.colorComponentValue.maxPotentialColorComponentValue);
        }
#endif
        return s;
    }

    std::string present(const std::uint8_t *data, std::size_t size, int width, int height,
                        const HdrMetadata &metadata) {
        if (QSize(width, height) != pixelSize()) {
            throw std::runtime_error("image size differs from the swap chain size");
        }
        const auto format = m_fp16 ? QRhiTexture::RGBA16F : QRhiTexture::RGB10A2;
        const std::size_t bytesPerPixel = m_fp16 ? 8 : 4;
        if (size != std::size_t(width) * std::size_t(height) * bytesPerPixel) {
            throw std::runtime_error("pixel buffer does not match the image size");
        }
        if (!m_rhi->isTextureFormatSupported(format)) {
            throw std::runtime_error("the GPU cannot sample the pattern texture format");
        }
        m_ps.reset();
        m_srb.reset();
        m_tex.reset(m_rhi->newTexture(format, QSize(width, height)));
        if (!m_tex->create()) {
            throw std::runtime_error("cannot create the pattern texture");
        }
        m_srb.reset(m_rhi->newShaderResourceBindings());
        m_srb->setBindings({QRhiShaderResourceBinding::sampledTexture(
            1, QRhiShaderResourceBinding::FragmentStage, m_tex.get(), m_sampler.get())});
        if (!m_srb->create()) {
            throw std::runtime_error("cannot create the shader resource bindings");
        }
        m_ps.reset(m_rhi->newGraphicsPipeline());
        m_ps->setShaderStages(
            {{QRhiShaderStage::Vertex, loadShader(kPatternVertQsb, sizeof kPatternVertQsb)},
             {QRhiShaderStage::Fragment, loadShader(kPatternFragQsb, sizeof kPatternFragQsb)}});
        m_ps->setVertexInputLayout({});
        m_ps->setShaderResourceBindings(m_srb.get());
        m_ps->setRenderPassDescriptor(m_rp.get());
        if (!m_ps->create()) {
            throw std::runtime_error("cannot create the graphics pipeline");
        }
        m_pending = QByteArray(reinterpret_cast<const char *>(data), qsizetype(size));
        std::string status = applyMetadata(metadata);
        requestUpdate();
        return status;
    }

protected:
    void exposeEvent(QExposeEvent *) override {
        if (!isExposed()) {
            return;
        }
        if (!m_ready && m_error.empty()) {
            // Qt event handlers must not throw.
            try {
                init();
                m_ready = true;
            } catch (const std::exception &e) {
                m_error = e.what();
            }
            if (m_onInit) {
                m_onInit();
            }
        }
        if (m_ready) {
            render();
        }
    }

    bool event(QEvent *e) override {
        switch (e->type()) {
        case QEvent::UpdateRequest:
            if (m_ready) {
                render();
            }
            return true;
        case QEvent::PlatformSurface:
            // The swap chain must go before the native surface.
            if (static_cast<QPlatformSurfaceEvent *>(e)->surfaceEventType() ==
                    QPlatformSurfaceEvent::SurfaceAboutToBeDestroyed &&
                m_sc) {
                m_sc->destroy();
                m_ready = false;
            }
            break;
        default:
            break;
        }
        return QWindow::event(e);
    }

    void keyPressEvent(QKeyEvent *e) override {
        switch (e->key()) {
        case Qt::Key_Escape:
        case Qt::Key_Q:
        case Qt::Key_Space:
            deleteLater();
            break;
        default:
            QWindow::keyPressEvent(e);
        }
    }

    void mouseDoubleClickEvent(QMouseEvent *) override { deleteLater(); }

private:
    void init() {
#if defined(Q_OS_WIN)
        QRhiD3D11InitParams params;
        QRhiD3D11NativeHandles adapter;
        adapter.adapterLuidLow = m_adapter.LowPart;
        adapter.adapterLuidHigh = m_adapter.HighPart;
        m_rhi.reset(QRhi::create(QRhi::D3D11, &params, {}, &adapter));
#elif defined(Q_OS_MACOS)
        QRhiMetalInitParams params;
        m_rhi.reset(QRhi::create(QRhi::Metal, &params));
#else
        QRhiVulkanInitParams params;
        params.inst = vulkanInstance();
        params.window = this;
        // Unsupported device extensions are ignored; applyMetadata checks.
        params.deviceExtensions = {"VK_EXT_hdr_metadata"};
        m_rhi.reset(QRhi::create(QRhi::Vulkan, &params));
#endif
        if (!m_rhi) {
            throw std::runtime_error("cannot create the QRhi device");
        }
        m_sc.reset(m_rhi->newSwapChain());
        m_sc->setWindow(this);
        const auto format = m_fp16 ? QRhiSwapChain::HDRExtendedSrgbLinear : QRhiSwapChain::HDR10;
        // Without this check QRhi falls back to an SDR swap chain silently.
        if (!m_sc->isFormatSupported(format)) {
            throw std::runtime_error(
                std::string(m_fp16 ? "FP16 scRGB" : "HDR10") +
                " swap chain not supported on this display; turn on HDR in the system "
                "display settings");
        }
        m_sc->setFormat(format);
        m_rp.reset(m_sc->newCompatibleRenderPassDescriptor());
        m_sc->setRenderPassDescriptor(m_rp.get());
        if (!m_sc->createOrResize()) {
            throw std::runtime_error("cannot create the swap chain");
        }
        m_sampler.reset(m_rhi->newSampler(QRhiSampler::Nearest, QRhiSampler::Nearest,
                                          QRhiSampler::None, QRhiSampler::ClampToEdge,
                                          QRhiSampler::ClampToEdge));
        if (!m_sampler->create()) {
            throw std::runtime_error("cannot create the sampler");
        }
    }

    void render() {
        if (m_sc->currentPixelSize() != m_sc->surfacePixelSize()) {
            // The pattern texture keeps its size; the shader draws it 1:1
            // from the top-left corner.
            m_sc->createOrResize();
        }
        QRhi::FrameOpResult r = m_rhi->beginFrame(m_sc.get());
        if (r == QRhi::FrameOpSwapChainOutOfDate) {
            m_sc->createOrResize();
            r = m_rhi->beginFrame(m_sc.get());
        }
        if (r != QRhi::FrameOpSuccess) {
            return;
        }
        QRhiResourceUpdateBatch *updates = nullptr;
        if (!m_pending.isEmpty()) {
            updates = m_rhi->nextResourceUpdateBatch();
            updates->uploadTexture(
                m_tex.get(),
                QRhiTextureUploadEntry(0, 0, QRhiTextureSubresourceUploadDescription(m_pending)));
            m_pending.clear();
        }
        QRhiCommandBuffer *cb = m_sc->currentFrameCommandBuffer();
        cb->beginPass(m_sc->currentFrameRenderTarget(), Qt::black, {1.0f, 0}, updates);
        if (m_ps) {
            const QSize s = m_sc->currentPixelSize();
            cb->setGraphicsPipeline(m_ps.get());
            cb->setViewport({0, 0, float(s.width()), float(s.height())});
            cb->setShaderResources();
            cb->draw(3);
        }
        cb->endPass();
        m_rhi->endFrame(m_sc.get());
    }

    // The swap chain of each window starts without metadata, so a disabled
    // metadata needs no call.
    std::string applyMetadata(const HdrMetadata &md) {
        if (!md.enabled) {
            return "No HDR metadata.";
        }
        const std::string values =
            sformat("MaxCLL %u, MaxFALL %u, mastering %.4f to %.0f cd/m2", unsigned(md.max_cll),
                   unsigned(md.max_fall), md.min_mastering / 10000.0, md.max_mastering / 10000.0);
#if defined(Q_OS_WIN)
        auto *sc = static_cast<QD3D11SwapChain *>(m_sc.get())->swapChain;
        ComPtr<IDXGISwapChain4> sc4;
        check(sc->QueryInterface(IID_PPV_ARGS(&sc4)), "IDXGISwapChain4");
        DXGI_HDR_METADATA_HDR10 d{};
        const auto &c = md.chromaticity;
        d.RedPrimary[0] = c[0], d.RedPrimary[1] = c[1];
        d.GreenPrimary[0] = c[2], d.GreenPrimary[1] = c[3];
        d.BluePrimary[0] = c[4], d.BluePrimary[1] = c[5];
        d.WhitePoint[0] = c[6], d.WhitePoint[1] = c[7];
        // DXGI documents the maximum in whole cd/m² and the minimum in
        // 0.0001 cd/m², unlike ST 2086, which uses 0.0001 for both.
        d.MaxMasteringLuminance = UINT(std::lround(md.max_mastering / 10000.0));
        d.MinMasteringLuminance = md.min_mastering;
        d.MaxContentLightLevel = md.max_cll;
        d.MaxFrameAverageLightLevel = md.max_fall;
        check(sc4->SetHDRMetaData(DXGI_HDR_METADATA_TYPE_HDR10, sizeof d, &d), "SetHDRMetaData");
        return "HDR10 metadata: " + values + ".";
#elif defined(Q_OS_MACOS)
        nitpick::setMetalHdrMetadata(this, md);
        return "EDR HDR10 metadata: " + values + ".";
#else
        const auto *handles = static_cast<const QRhiVulkanNativeHandles *>(m_rhi->nativeHandles());
        auto getDeviceProcAddr = reinterpret_cast<PFN_vkGetDeviceProcAddr>(
            vulkanInstance()->getInstanceProcAddr("vkGetDeviceProcAddr"));
        auto setHdrMetadata =
            getDeviceProcAddr ? reinterpret_cast<PFN_vkSetHdrMetadataEXT>(
                                    getDeviceProcAddr(handles->dev, "vkSetHdrMetadataEXT"))
                              : nullptr;
        if (!setHdrMetadata) {
            return "HDR metadata NOT sent: the Vulkan driver has no VK_EXT_hdr_metadata.";
        }
        const auto &c = md.chromaticity;
        const auto xy = [&c](int i) { return VkXYColorEXT{c[i] / 50000.0f, c[i + 1] / 50000.0f}; };
        VkHdrMetadataEXT d{};
        d.sType = VK_STRUCTURE_TYPE_HDR_METADATA_EXT;
        d.displayPrimaryRed = xy(0);
        d.displayPrimaryGreen = xy(2);
        d.displayPrimaryBlue = xy(4);
        d.whitePoint = xy(6);
        d.maxLuminance = md.max_mastering / 10000.0f;
        d.minLuminance = md.min_mastering / 10000.0f;
        d.maxContentLightLevel = md.max_cll;
        d.maxFrameAverageLightLevel = md.max_fall;
        VkSwapchainKHR sc = static_cast<QVkSwapChain *>(m_sc.get())->sc;
        setHdrMetadata(handles->dev, 1, &sc, &d);
        return "HDR10 metadata: " + values + ".";
#endif
    }

    bool m_fp16;
    bool m_ready = false;
    std::string m_error;
    std::function<void()> m_onInit;
    QByteArray m_pending;
#if defined(Q_OS_WIN)
    LUID m_adapter{};
    ComPtr<IDXGIOutput6> m_output;
#endif
    // Declared first, destroyed last: every QRhi resource needs the QRhi.
    std::unique_ptr<QRhi> m_rhi;
    std::unique_ptr<QRhiSwapChain> m_sc;
    std::unique_ptr<QRhiRenderPassDescriptor> m_rp;
    std::unique_ptr<QRhiSampler> m_sampler;
    std::unique_ptr<QRhiTexture> m_tex;
    std::unique_ptr<QRhiShaderResourceBindings> m_srb;
    std::unique_ptr<QRhiGraphicsPipeline> m_ps;
};

QPointer<PatternWindow> g_window;

} // namespace

namespace nitpick {

std::int32_t screenPixelWidth(std::int32_t screen) {
#if defined(Q_OS_WIN)
    const RECT r = monitorRect(screenAt(screen));
    return r.right - r.left;
#else
    QScreen *s = screenAt(screen);
    return std::int32_t(std::lround(s->geometry().width() * s->devicePixelRatio()));
#endif
}

std::int32_t screenPixelHeight(std::int32_t screen) {
#if defined(Q_OS_WIN)
    const RECT r = monitorRect(screenAt(screen));
    return r.bottom - r.top;
#else
    QScreen *s = screenAt(screen);
    return std::int32_t(std::lround(s->geometry().height() * s->devicePixelRatio()));
#endif
}

PatternSurface openPatternWindow(std::int32_t screen, bool fp16) {
    QScreen *target = screenAt(screen);
    delete g_window.data();
    auto window = std::make_unique<PatternWindow>(target, fp16);
    window->showFullScreen();
    window->requestActivate();
    window->waitUntilReady();
    PatternSurface surface;
    surface.width = window->pixelSize().width();
    surface.height = window->pixelSize().height();
    surface.description = rust::String(window->describe());
    g_window = window.release();
    return surface;
}

rust::String presentPattern(rust::Slice<const std::uint8_t> pixels, std::int32_t width,
                            std::int32_t height, const HdrMetadata &metadata) {
    if (!g_window) {
        throw std::runtime_error("no pattern window is open");
    }
    return rust::String(g_window->present(pixels.data(), pixels.size(), width, height, metadata));
}

} // namespace nitpick
