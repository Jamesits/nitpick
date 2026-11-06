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

#include <algorithm>
#include <array>
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
#include "nitpick-app/cpp/macos.h"
#else
#include <QtGui/QVulkanInstance>
#include <QtGui/private/qrhivulkan_p.h>
#endif

namespace {

using nitpick::DxgiHdrMetadata;
using nitpick::MetalHdrMetadata;
using nitpick::VulkanHdrMetadata;
using nitpick::ScreenIdentity;

// Time for the compositor to map the window and apply the full-screen state.
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

// GDI name of the monitor, for example `\\.\DISPLAY1`. The Rust adapter
// maps it to a display device; capabilities come from that device report.
std::string gdiName(QScreen *screen) {
    MONITORINFOEXW info{};
    info.cbSize = sizeof info;
    if (!GetMonitorInfoW(monitorOf(screen), &info)) {
        throw std::runtime_error("GetMonitorInfoW failed");
    }
    char name[64];
    std::snprintf(name, sizeof name, "%ls", info.szDevice);
    return name;
}

// The adapter that scans out `monitor`. Rendering on that adapter avoids a
// cross-adapter copy on hybrid-GPU systems.
LUID adapterOf(HMONITOR monitor) {
    ComPtr<IDXGIFactory1> factory;
    check(CreateDXGIFactory1(IID_PPV_ARGS(&factory)), "CreateDXGIFactory1");
    ComPtr<IDXGIAdapter1> adapter;
    for (UINT a = 0; factory->EnumAdapters1(a, &adapter) != DXGI_ERROR_NOT_FOUND; ++a) {
        ComPtr<IDXGIOutput> output;
        for (UINT o = 0; adapter->EnumOutputs(o, &output) != DXGI_ERROR_NOT_FOUND; ++o) {
            DXGI_OUTPUT_DESC desc{};
            if (SUCCEEDED(output->GetDesc(&desc)) && desc.Monitor == monitor) {
                DXGI_ADAPTER_DESC1 ad{};
                check(adapter->GetDesc1(&ad), "GetDesc1");
                return ad.AdapterLuid;
            }
        }
    }
    throw std::runtime_error("no DXGI output for the selected screen");
}

#elif !defined(Q_OS_MACOS)

// Created once and never destroyed: windows that use it may live until the
// application exits. Not named `vulkanInstance`: inside a QWindow subclass
// that name finds QWindow::vulkanInstance(), which returns null here.
QVulkanInstance *sharedVulkanInstance() {
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

// Pixel size of the pattern area of a full-screen window on `screen`. The
// launcher renders reference tables at this size before the window exists.
QSize screenPatternSize(QScreen *screen) {
#if defined(Q_OS_WIN)
    const RECT r = monitorRect(screen);
    return QSize(r.right - r.left, r.bottom - r.top);
#elif defined(Q_OS_MACOS)
    return nitpick::screenSafePixelSize(screen);
#else
    // QScreen has no safe-area insets.
    return QSize(int(std::lround(screen->geometry().width() * screen->devicePixelRatio())),
                 int(std::lround(screen->geometry().height() * screen->devicePixelRatio())));
#endif
}

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
    PatternWindow(QScreen *screen, bool fp16)
        : m_fp16(fp16), m_expectedSize(screenPatternSize(screen)) {
#if defined(Q_OS_WIN)
        setSurfaceType(QSurface::Direct3DSurface);
        m_adapter = adapterOf(monitorOf(screen));
#elif defined(Q_OS_MACOS)
        setSurfaceType(QSurface::MetalSurface);
#else
        setSurfaceType(QSurface::VulkanSurface);
        setVulkanInstance(sharedVulkanInstance());
#endif
        setFlags(Qt::Window | Qt::FramelessWindowHint);
        setScreen(screen);
        setGeometry(screen->geometry());
        setCursor(Qt::BlankCursor);
        setTitle(QStringLiteral("nitpick pattern"));
    }

    // Closing or replacing the window fails a pending frame with
    // Disconnected.
    ~PatternWindow() override { nitpick::frameWindowClosed(); }

    // Blocks until the swap chain exists and the pattern area has the size of
    // the reference table. GNOME on Wayland first exposes the window at the
    // work-area size and applies the full-screen state later. After the
    // timeout, a swap chain of a different size is used; describe() warns.
    void waitUntilReady() {
        if (!settled() && m_error.empty()) {
            QEventLoop loop;
            m_onSettled = [&loop] { loop.quit(); };
            QTimer::singleShot(kInitTimeoutMs, &loop, &QEventLoop::quit);
            loop.exec(QEventLoop::ExcludeUserInputEvents);
            m_onSettled = nullptr;
        }
        if (!m_error.empty()) {
            throw std::runtime_error(m_error);
        }
        if (!m_ready) {
            throw std::runtime_error("the pattern window was not exposed in time");
        }
    }

    // The swap chain without the safe-area margins of the window, in pixels.
    // The pattern is drawn only inside this area; the rest stays black.
    QRect patternArea() const {
        const QSize s = m_sc->currentPixelSize();
        const QMargins m = safeAreaMargins();
        const qreal dpr = devicePixelRatio();
        const auto px = [dpr](int v) { return int(std::lround(v * dpr)); };
        return QRect(QPoint(px(m.left()), px(m.top())),
                     QPoint(s.width() - px(m.right()) - 1, s.height() - px(m.bottom()) - 1));
    }

    // Swap chain and backend only. The display state comes from the device
    // report of the Rust adapter, not from QRhi's hdrInfo() or DXGI here.
    std::string describe() const {
        std::string s = m_fp16 ? "FP16 scRGB swap chain" : "HDR10 swap chain";
        s += " (" + std::string(m_rhi->backendName()) + ")";
        if (patternArea().size() != m_expectedSize) {
            s += "; WARNING: the pattern area differs from the screen size of the reference table";
        }
        return s;
    }

    // Empty if QRhi accepted the swap chain format; see init().
    const std::string &fallback() const { return m_fallback; }

    // Queues the frame and returns; render() uploads it and reports it by
    // frame ID. A frame queued before the render replaces this one, so
    // late pixels never upload over a newer request.
    void present(const std::uint8_t *data, std::size_t size, int width, int height,
                 std::uint64_t frame, int timeoutMs) {
        if (QSize(width, height) != patternArea().size()) {
            throw std::runtime_error("image size differs from the pattern area size");
        }
        const std::size_t bytesPerPixel = m_fp16 ? 8 : 4;
        if (size != std::size_t(width) * std::size_t(height) * bytesPerPixel) {
            throw std::runtime_error("pixel buffer does not match the image size");
        }
        if (!m_rhi->isTextureFormatSupported(textureFormat())) {
            throw std::runtime_error("the GPU cannot sample the pattern texture format");
        }
        m_pending = QByteArray(reinterpret_cast<const char *>(data), qsizetype(size));
        m_pendingSize = QSize(width, height);
        requestUpdate();
        // The Rust side ignores the timer of a frame that was already
        // uploaded or replaced.
        QTimer::singleShot(timeoutMs, this, [frame] { nitpick::frameTimedOut(frame); });
    }

    // Each metadata call applies at the next present of the swap chain.
#if defined(Q_OS_WIN)
    void setMetadata(const DxgiHdrMetadata &md) {
        DXGI_HDR_METADATA_HDR10 d{};
        std::copy_n(md.red_primary.data(), 2, d.RedPrimary);
        std::copy_n(md.green_primary.data(), 2, d.GreenPrimary);
        std::copy_n(md.blue_primary.data(), 2, d.BluePrimary);
        std::copy_n(md.white_point.data(), 2, d.WhitePoint);
        d.MaxMasteringLuminance = md.max_mastering_luminance;
        d.MinMasteringLuminance = md.min_mastering_luminance;
        d.MaxContentLightLevel = md.max_content_light_level;
        d.MaxFrameAverageLightLevel = md.max_frame_average_light_level;
        check(swapChain4()->SetHDRMetaData(DXGI_HDR_METADATA_TYPE_HDR10, sizeof d, &d),
              "SetHDRMetaData");
    }

    void clearMetadata() {
        check(swapChain4()->SetHDRMetaData(DXGI_HDR_METADATA_TYPE_NONE, 0, nullptr),
              "SetHDRMetaData(NONE)");
    }
#elif defined(Q_OS_MACOS)
    void setMetadata(const MetalHdrMetadata &md) { nitpick::setLayerHdrMetadata(this, &md); }

    void clearMetadata() { nitpick::setLayerHdrMetadata(this, nullptr); }
#else
    void setMetadata(const VulkanHdrMetadata &md) {
        const auto xy = [](const std::array<float, 2> &v) { return VkXYColorEXT{v[0], v[1]}; };
        VkHdrMetadataEXT d{};
        d.sType = VK_STRUCTURE_TYPE_HDR_METADATA_EXT;
        d.displayPrimaryRed = xy(md.display_primary_red);
        d.displayPrimaryGreen = xy(md.display_primary_green);
        d.displayPrimaryBlue = xy(md.display_primary_blue);
        d.whitePoint = xy(md.white_point);
        d.maxLuminance = md.max_luminance;
        d.minLuminance = md.min_luminance;
        d.maxContentLightLevel = md.max_content_light_level;
        d.maxFrameAverageLightLevel = md.max_frame_average_light_level;
        const auto *handles = static_cast<const QRhiVulkanNativeHandles *>(m_rhi->nativeHandles());
        auto getDeviceProcAddr = reinterpret_cast<PFN_vkGetDeviceProcAddr>(
            sharedVulkanInstance()->getInstanceProcAddr("vkGetDeviceProcAddr"));
        auto setHdrMetadata =
            getDeviceProcAddr ? reinterpret_cast<PFN_vkSetHdrMetadataEXT>(
                                    getDeviceProcAddr(handles->dev, "vkSetHdrMetadataEXT"))
                              : nullptr;
        if (!setHdrMetadata) {
            throw std::runtime_error("the Vulkan driver has no VK_EXT_hdr_metadata");
        }
        VkSwapchainKHR sc = static_cast<QVkSwapChain *>(m_sc.get())->sc;
        setHdrMetadata(handles->dev, 1, &sc, &d);
    }

    void clearMetadata() {
        throw std::runtime_error("VK_EXT_hdr_metadata has no call to remove metadata");
    }
#endif

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
        }
        if (m_ready) {
            render();
        }
        notifyIfSettled();
    }

    bool event(QEvent *e) override {
        switch (e->type()) {
        case QEvent::UpdateRequest:
            if (m_ready) {
                render();
            }
            notifyIfSettled();
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
    bool settled() const { return m_ready && patternArea().size() == m_expectedSize; }

    void notifyIfSettled() {
        if (m_onSettled && (!m_error.empty() || settled())) {
            m_onSettled();
        }
    }

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
        params.inst = sharedVulkanInstance();
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
        // QRhi changes an unsupported format without an error. The window
        // still shows the pattern, so that the user can see the remapped
        // output; the launcher shows the fallback as a warning.
        if (!m_sc->isFormatSupported(format)) {
            // Qt 6.12 behaviour of each backend's createOrResize().
#if defined(Q_OS_WIN)
            m_fallback = "D3D11 uses an 8-bit SDR swap chain (sRGB, BT.709); the codes are shown "
                         "as SDR values";
#elif defined(Q_OS_MACOS)
            m_fallback = "Metal keeps the format, but the display has no EDR headroom; macOS "
                         "clips at SDR white";
#else
            m_fallback = "Vulkan uses the first surface format of the compositor; the codes are "
                         "shown as SDR values";
#endif
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
        // std140 `ivec2 origin`, padded to 16 bytes.
        m_area.reset(m_rhi->newBuffer(QRhiBuffer::Dynamic, QRhiBuffer::UniformBuffer, 16));
        if (!m_area->create()) {
            throw std::runtime_error("cannot create the pattern area buffer");
        }
    }

    QRhiTexture::Format textureFormat() const {
        return m_fp16 ? QRhiTexture::RGBA16F : QRhiTexture::RGB10A2;
    }

    // Texture, bindings and pipeline for an image of `size`. Builds new
    // objects first, so that a failure keeps the previous pattern.
    void preparePattern(QSize size) {
        std::unique_ptr<QRhiTexture> tex(m_rhi->newTexture(textureFormat(), size));
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
        ps->setRenderPassDescriptor(m_rp.get());
        if (!ps->create()) {
            throw std::runtime_error("cannot create the graphics pipeline");
        }
        m_ps = std::move(ps);
        m_srb = std::move(srb);
        m_tex = std::move(tex);
    }

    void render() {
        if (m_sc->currentPixelSize() != m_sc->surfacePixelSize()) {
            // The pattern texture keeps its size; the shader draws it 1:1
            // from the top-left corner of the pattern area.
            m_sc->createOrResize();
        }
        QRhi::FrameOpResult r = m_rhi->beginFrame(m_sc.get());
        if (r == QRhi::FrameOpSwapChainOutOfDate) {
            m_sc->createOrResize();
            r = m_rhi->beginFrame(m_sc.get());
        }
        if (r == QRhi::FrameOpSwapChainOutOfDate) {
            // Transient, for example during a mode change. The queued
            // frame retries until its timeout.
            if (!m_pending.isEmpty()) {
                requestUpdate();
            }
            return;
        }
        if (r != QRhi::FrameOpSuccess) {
            m_pending.clear();
            nitpick::frameFailed(frameOpError("beginFrame", r));
            return;
        }
        QRhiResourceUpdateBatch *updates = m_rhi->nextResourceUpdateBatch();
        // The safe-area margins can change after the first expose.
        const QPoint origin = patternArea().topLeft();
        const std::int32_t area[2] = {origin.x(), origin.y()};
        updates->updateDynamicBuffer(m_area.get(), 0, sizeof area, area);
        // 0 if no frame is queued, or if the queued frame timed out: its
        // pixels must not appear after the host got the error.
        const std::uint64_t frame = nitpick::frameTakeUpload();
        std::string error;
        if (frame != 0) {
            try {
                preparePattern(m_pendingSize);
                updates->uploadTexture(
                    m_tex.get(), QRhiTextureUploadEntry(
                                     0, 0, QRhiTextureSubresourceUploadDescription(m_pending)));
            } catch (const std::exception &e) {
                error = e.what();
            }
        }
        m_pending.clear();
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
        r = m_rhi->endFrame(m_sc.get());
        if (frame != 0) {
            if (error.empty() && r != QRhi::FrameOpSuccess) {
                error = frameOpError("endFrame", r);
            }
            nitpick::frameEnded(frame, error);
        }
    }

    static std::string frameOpError(const char *call, QRhi::FrameOpResult r) {
        switch (r) {
        case QRhi::FrameOpDeviceLost:
            return std::string(call) + " failed: graphics device lost";
        case QRhi::FrameOpSwapChainOutOfDate:
            return std::string(call) + " failed: swap chain out of date";
        default:
            return std::string(call) + " failed";
        }
    }

#if defined(Q_OS_WIN)
    ComPtr<IDXGISwapChain4> swapChain4() const {
        auto *sc = static_cast<QD3D11SwapChain *>(m_sc.get())->swapChain;
        ComPtr<IDXGISwapChain4> sc4;
        check(sc->QueryInterface(IID_PPV_ARGS(&sc4)), "IDXGISwapChain4");
        return sc4;
    }
#endif

    bool m_fp16;
    // Pattern area size of the reference table, see screenPatternSize().
    QSize m_expectedSize;
    bool m_ready = false;
    // The swap chain that QRhi uses instead of the requested format; see init().
    std::string m_fallback;
    std::string m_error;
    std::function<void()> m_onSettled;
    // Pixels of the queued frame; its ID is in the Rust submission state.
    QByteArray m_pending;
    QSize m_pendingSize;
#if defined(Q_OS_WIN)
    LUID m_adapter{};
#endif
    // Declared first, destroyed last: every QRhi resource needs the QRhi.
    std::unique_ptr<QRhi> m_rhi;
    std::unique_ptr<QRhiSwapChain> m_sc;
    std::unique_ptr<QRhiRenderPassDescriptor> m_rp;
    std::unique_ptr<QRhiSampler> m_sampler;
    std::unique_ptr<QRhiBuffer> m_area;
    std::unique_ptr<QRhiTexture> m_tex;
    std::unique_ptr<QRhiShaderResourceBindings> m_srb;
    std::unique_ptr<QRhiGraphicsPipeline> m_ps;
};

QPointer<PatternWindow> g_window;

PatternWindow &openWindow() {
    if (!g_window) {
        throw std::runtime_error("no pattern window is open");
    }
    return *g_window;
}

} // namespace

namespace nitpick {

std::int32_t screenCount() { return std::int32_t(QGuiApplication::screens().size()); }

std::int32_t screenPixelWidth(std::int32_t screen) {
    return screenPatternSize(screenAt(screen)).width();
}

std::int32_t screenPixelHeight(std::int32_t screen) {
    return screenPatternSize(screenAt(screen)).height();
}

ScreenIdentity screenIdentity(std::int32_t screen) {
    QScreen *s = screenAt(screen);
    ScreenIdentity id{};
    id.name = rust::String(s->name().toStdString());
#if defined(Q_OS_WIN)
    id.gdi_name = rust::String(gdiName(s));
#elif defined(Q_OS_MACOS)
    id.display_id = displayIdOf(s);
#endif
    return id;
}

PatternSurface openPatternWindow(std::int32_t screen, bool fp16) {
    QScreen *target = screenAt(screen);
    delete g_window.data();
    auto window = std::make_unique<PatternWindow>(target, fp16);
    window->showFullScreen();
    window->requestActivate();
    window->waitUntilReady();
    PatternSurface surface{};
    surface.fallback = rust::String(window->fallback());
    surface.width = window->patternArea().width();
    surface.height = window->patternArea().height();
    surface.description = rust::String(window->describe());
    g_window = window.release();
    return surface;
}

void closePatternWindow() { delete g_window.data(); }

void presentPattern(rust::Slice<const std::uint8_t> pixels, std::int32_t width,
                    std::int32_t height, std::uint64_t frame, std::int32_t timeoutMs) {
    openWindow().present(pixels.data(), pixels.size(), width, height, frame, timeoutMs);
}

void setDxgiHdrMetadata(const DxgiHdrMetadata &metadata) {
#if defined(Q_OS_WIN)
    openWindow().setMetadata(metadata);
#else
    (void)metadata;
    throw std::runtime_error("DXGI metadata needs the D3D11 backend");
#endif
}

void setVulkanHdrMetadata(const VulkanHdrMetadata &metadata) {
#if !defined(Q_OS_WIN) && !defined(Q_OS_MACOS)
    openWindow().setMetadata(metadata);
#else
    (void)metadata;
    throw std::runtime_error("Vulkan metadata needs the Vulkan backend");
#endif
}

void setMetalHdrMetadata(const MetalHdrMetadata &metadata) {
#if defined(Q_OS_MACOS)
    openWindow().setMetadata(metadata);
#else
    (void)metadata;
    throw std::runtime_error("EDR metadata needs the Metal backend");
#endif
}

void clearHdrMetadata() { openWindow().clearMetadata(); }

} // namespace nitpick
