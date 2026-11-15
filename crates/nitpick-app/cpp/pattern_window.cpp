#include "nitpick-app/cpp/pattern_window.h"
#include "nitpick-app/cpp/pattern_renderer.h"
#include "nitpick-app/cpp/swap_chain_probe.h"

#include "nitpick-app/src/display.cxx.h"
#include "nitpick-app/src/swap_chain_probe.cxx.h"

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
#include <cstddef>
#include <cstdio>
#include <functional>
#include <memory>
#include <optional>
#include <stdexcept>
#include <string>
#include <vector>

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
#elif !defined(NITPICK_HAS_VULKAN)
#error "the pattern window needs Vulkan on this platform: Qt with Vulkan and vulkan/vulkan.h"
#endif

// Windows has Vulkan in addition to D3D11 if the build finds the headers.
#if defined(NITPICK_HAS_VULKAN) && !defined(Q_OS_MACOS)
#define NITPICK_WINDOW_VULKAN 1
#include <QtGui/private/qrhivulkan_p.h>
#endif

namespace {

using nitpick::DxgiHdrMetadata;
using nitpick::FrameFault;
using nitpick::MetalHdrMetadata;
using nitpick::PatternBackend;
using nitpick::ScreenIdentity;
using nitpick::VulkanHdrMetadata;

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

const char *textureFormatName(QRhiTexture::Format f) {
    switch (f) {
    case QRhiTexture::RGB10A2:
        return "RGB10A2";
    case QRhiTexture::RGBA16F:
        return "RGBA16F";
    case QRhiTexture::RGBA32F:
        return "RGBA32F";
    case QRhiTexture::RGBA8:
        return "RGBA8";
    case QRhiTexture::BGRA8:
        return "BGRA8";
    case QRhiTexture::UnknownFormat:
        return "unknown";
    default:
        return "other";
    }
}

// Test probe state, see swap_chain_probe.h. It applies to the open window.
struct ProbeImage {
    std::uint64_t frame = 0;
    QRhiTexture::Format format = QRhiTexture::UnknownFormat;
    QByteArray data;
    QSize size;
    bool yUp = false;
    std::string error;
};

struct Probe {
    bool readback = false;
    bool hold = false;
    FrameFault fault = FrameFault::None;
    std::uint32_t faultCount = 0;
    std::vector<std::uint64_t> queued;
    std::vector<std::uint64_t> uploaded;
    std::optional<ProbeImage> image;
    std::optional<nitpick::MetadataCall> metadata;

    // Consumes one injected fault of `kind`.
    bool take(FrameFault kind) {
        if (fault != kind || faultCount == 0) {
            return false;
        }
        --faultCount;
        return true;
    }

    void recordMetadata(const char *api, const void *data, std::size_t size,
                        const std::string &error) {
        nitpick::MetadataCall call{};
        call.api = rust::String(api);
        const auto *bytes = static_cast<const std::uint8_t *>(data);
        for (std::size_t i = 0; i < size; ++i) {
            call.data.push_back(bytes[i]);
        }
        call.error = rust::String(error);
        metadata = std::move(call);
    }
};

Probe g_probe;

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

std::string dxgiFormatName(DXGI_FORMAT f) {
    switch (f) {
    case DXGI_FORMAT_R10G10B10A2_UNORM:
        return "DXGI_FORMAT_R10G10B10A2_UNORM";
    case DXGI_FORMAT_R16G16B16A16_FLOAT:
        return "DXGI_FORMAT_R16G16B16A16_FLOAT";
    case DXGI_FORMAT_R8G8B8A8_UNORM:
        return "DXGI_FORMAT_R8G8B8A8_UNORM";
    case DXGI_FORMAT_B8G8R8A8_UNORM:
        return "DXGI_FORMAT_B8G8R8A8_UNORM";
    default:
        return "DXGI_FORMAT " + std::to_string(int(f));
    }
}

const char *dxgiColourSpaceName(DXGI_COLOR_SPACE_TYPE c) {
    switch (c) {
    case DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020:
        return "DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020";
    case DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709:
        return "DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709";
    default:
        return "DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709";
    }
}

#endif

#if defined(NITPICK_WINDOW_VULKAN)

std::string vkFormatName(VkFormat f) {
    switch (f) {
    case VK_FORMAT_A2B10G10R10_UNORM_PACK32:
        return "VK_FORMAT_A2B10G10R10_UNORM_PACK32";
    case VK_FORMAT_A2R10G10B10_UNORM_PACK32:
        return "VK_FORMAT_A2R10G10B10_UNORM_PACK32";
    case VK_FORMAT_R16G16B16A16_SFLOAT:
        return "VK_FORMAT_R16G16B16A16_SFLOAT";
    case VK_FORMAT_B8G8R8A8_UNORM:
        return "VK_FORMAT_B8G8R8A8_UNORM";
    case VK_FORMAT_B8G8R8A8_SRGB:
        return "VK_FORMAT_B8G8R8A8_SRGB";
    case VK_FORMAT_R8G8B8A8_UNORM:
        return "VK_FORMAT_R8G8B8A8_UNORM";
    default:
        return "VkFormat " + std::to_string(int(f));
    }
}

std::string vkColourSpaceName(VkColorSpaceKHR c) {
    switch (c) {
    case VK_COLOR_SPACE_SRGB_NONLINEAR_KHR:
        return "VK_COLOR_SPACE_SRGB_NONLINEAR_KHR";
    case VK_COLOR_SPACE_HDR10_ST2084_EXT:
        return "VK_COLOR_SPACE_HDR10_ST2084_EXT";
    case VK_COLOR_SPACE_EXTENDED_SRGB_LINEAR_EXT:
        return "VK_COLOR_SPACE_EXTENDED_SRGB_LINEAR_EXT";
    case VK_COLOR_SPACE_BT2020_LINEAR_EXT:
        return "VK_COLOR_SPACE_BT2020_LINEAR_EXT";
    default:
        return "VkColorSpaceKHR " + std::to_string(int(c));
    }
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

// The native swap chain after QRhi created it.
struct NativeSwapChain {
    std::string format;
    std::string colourSpace;
    bool matchesRequest = false;
};

// Borderless full-screen window that scans out one static image through
// QRhi: D3D11 or Vulkan on Windows, Metal on macOS, Vulkan elsewhere.
//
// The swap chain is HDR10 (10-bit unorm, PQ, BT.2020) or FP16 scRGB. The
// fragment shader copies one texel per pixel with texelFetch: no sampling,
// blending or colour conversion inside the application. The compositor and
// the driver may still alter values after the present, which is what the
// external scope verifies.
class PatternWindow final : public QWindow {
public:
    PatternWindow(QScreen *screen, bool fp16, PatternBackend backend)
        : m_fp16(fp16), m_backend(backend), m_expectedSize(screenPatternSize(screen)) {
        switch (backend) {
        case PatternBackend::D3d11:
#if defined(Q_OS_WIN)
            setSurfaceType(QSurface::Direct3DSurface);
            m_adapter = adapterOf(monitorOf(screen));
            break;
#else
            throw std::runtime_error("the D3D11 pattern window needs Windows");
#endif
        case PatternBackend::Metal:
#if defined(Q_OS_MACOS)
            setSurfaceType(QSurface::MetalSurface);
            break;
#else
            throw std::runtime_error("the Metal pattern window needs macOS");
#endif
        case PatternBackend::Vulkan:
#if defined(NITPICK_WINDOW_VULKAN)
            setSurfaceType(QSurface::VulkanSurface);
            setVulkanInstance(nitpick::sharedVulkanInstance());
            break;
#else
            throw std::runtime_error(
                "built without the Vulkan pattern window: Qt has no Vulkan support, the "
                "build found no vulkan/vulkan.h (set VULKAN_SDK), or the OS is macOS");
#endif
        default:
            throw std::runtime_error("unknown QRhi backend");
        }
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
        s += " (" + std::string(m_rhi->backendName()) + ": " + m_native.format + ", " +
             m_native.colourSpace + ")";
        if (patternArea().size() != m_expectedSize) {
            s += "; WARNING: the pattern area differs from the screen size of the reference table";
        }
        return s;
    }

    // Empty if QRhi accepted the swap chain format; see init().
    const std::string &fallback() const { return m_fallback; }

    nitpick::SwapChainRecord record() const {
        nitpick::SwapChainRecord r{};
        r.backend = rust::String(m_rhi->backendName());
        r.native_format = rust::String(m_native.format);
        r.colour_space = rust::String(m_native.colourSpace);
        r.matches_request = m_native.matchesRequest;
        r.fallback = rust::String(m_fallback);
        const QSize size = m_sc->currentPixelSize();
        r.width = size.width();
        r.height = size.height();
        const QRect area = patternArea();
        r.area_x = area.x();
        r.area_y = area.y();
        r.area_width = area.width();
        r.area_height = area.height();
        r.readback = m_readback;
        return r;
    }

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
        if (!m_rhi->isTextureFormatSupported(nitpick::patternTextureFormat(m_fp16))) {
            throw std::runtime_error("the GPU cannot sample the pattern texture format");
        }
        m_pending = QByteArray(reinterpret_cast<const char *>(data), qsizetype(size));
        m_pendingSize = QSize(width, height);
        g_probe.queued.push_back(frame);
        requestUpdate();
        // The Rust side ignores the timer of a frame that was already
        // uploaded or replaced.
        QTimer::singleShot(timeoutMs, this, [frame] { nitpick::frameTimedOut(frame); });
    }

    // Each metadata call applies at the next present of the swap chain.
    void setMetadata(const DxgiHdrMetadata &md) {
#if defined(Q_OS_WIN)
        requireBackend(PatternBackend::D3d11, "DXGI metadata");
        DXGI_HDR_METADATA_HDR10 d{};
        std::copy_n(md.red_primary.data(), 2, d.RedPrimary);
        std::copy_n(md.green_primary.data(), 2, d.GreenPrimary);
        std::copy_n(md.blue_primary.data(), 2, d.BluePrimary);
        std::copy_n(md.white_point.data(), 2, d.WhitePoint);
        d.MaxMasteringLuminance = md.max_mastering_luminance;
        d.MinMasteringLuminance = md.min_mastering_luminance;
        d.MaxContentLightLevel = md.max_content_light_level;
        d.MaxFrameAverageLightLevel = md.max_frame_average_light_level;
        recordCall("SetHDRMetaData(HDR10)", &d, sizeof d, [&] {
            check(swapChain4()->SetHDRMetaData(DXGI_HDR_METADATA_TYPE_HDR10, sizeof d, &d),
                  "SetHDRMetaData");
        });
#else
        (void)md;
        throw std::runtime_error("DXGI metadata needs the D3D11 backend");
#endif
    }

    void setMetadata(const VulkanHdrMetadata &md) {
#if defined(NITPICK_WINDOW_VULKAN)
        requireBackend(PatternBackend::Vulkan, "Vulkan metadata");
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
        const auto *fields = reinterpret_cast<const std::uint8_t *>(&d.displayPrimaryRed);
        const std::size_t size =
            offsetof(VkHdrMetadataEXT, maxFrameAverageLightLevel) + sizeof(float) -
            offsetof(VkHdrMetadataEXT, displayPrimaryRed);
        recordCall("vkSetHdrMetadataEXT", fields, size, [&] {
            const auto *handles =
                static_cast<const QRhiVulkanNativeHandles *>(m_rhi->nativeHandles());
            auto getDeviceProcAddr = reinterpret_cast<PFN_vkGetDeviceProcAddr>(
                nitpick::sharedVulkanInstance()->getInstanceProcAddr("vkGetDeviceProcAddr"));
            auto setHdrMetadata =
                getDeviceProcAddr ? reinterpret_cast<PFN_vkSetHdrMetadataEXT>(
                                        getDeviceProcAddr(handles->dev, "vkSetHdrMetadataEXT"))
                                  : nullptr;
            if (!setHdrMetadata) {
                throw std::runtime_error("the Vulkan driver has no VK_EXT_hdr_metadata");
            }
            VkSwapchainKHR sc = static_cast<QVkSwapChain *>(m_sc.get())->sc;
            setHdrMetadata(handles->dev, 1, &sc, &d);
        });
#else
        (void)md;
        throw std::runtime_error("Vulkan metadata needs the Vulkan backend");
#endif
    }

    void setMetadata(const MetalHdrMetadata &md) {
#if defined(Q_OS_MACOS)
        requireBackend(PatternBackend::Metal, "EDR metadata");
        std::vector<std::uint8_t> bytes(md.display_info.begin(), md.display_info.end());
        bytes.insert(bytes.end(), md.content_info.begin(), md.content_info.end());
        const auto *scale = reinterpret_cast<const std::uint8_t *>(&md.optical_output_scale);
        bytes.insert(bytes.end(), scale, scale + sizeof md.optical_output_scale);
        recordCall("CAEDRMetadata", bytes.data(), bytes.size(),
                   [&] { nitpick::setLayerHdrMetadata(this, &md); });
#else
        (void)md;
        throw std::runtime_error("EDR metadata needs the Metal backend");
#endif
    }

    void clearMetadata() {
        switch (m_backend) {
        case PatternBackend::D3d11:
#if defined(Q_OS_WIN)
            recordCall("SetHDRMetaData(NONE)", nullptr, 0, [&] {
                check(swapChain4()->SetHDRMetaData(DXGI_HDR_METADATA_TYPE_NONE, 0, nullptr),
                      "SetHDRMetaData(NONE)");
            });
            return;
#else
            break;
#endif
        case PatternBackend::Metal:
#if defined(Q_OS_MACOS)
            recordCall("EDRMetadata = nil", nullptr, 0,
                       [&] { nitpick::setLayerHdrMetadata(this, nullptr); });
            return;
#else
            break;
#endif
        case PatternBackend::Vulkan:
            throw std::runtime_error("VK_EXT_hdr_metadata has no call to remove metadata");
        default:
            break;
        }
        throw std::runtime_error("unknown QRhi backend");
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

    void requireBackend(PatternBackend backend, const char *what) const {
        if (m_backend != backend) {
            throw std::runtime_error(std::string(what) + " needs another QRhi backend");
        }
    }

    // Records the native structure for the probe, then makes the call.
    template <typename Call>
    void recordCall(const char *api, const void *data, std::size_t size, Call call) {
        try {
            call();
        } catch (const std::exception &e) {
            g_probe.recordMetadata(api, data, size, e.what());
            throw;
        }
        g_probe.recordMetadata(api, data, size, {});
    }

    std::unique_ptr<QRhi> createRhi() {
        switch (m_backend) {
#if defined(Q_OS_WIN)
        case PatternBackend::D3d11: {
            QRhiD3D11InitParams params;
            QRhiD3D11NativeHandles adapter;
            adapter.adapterLuidLow = m_adapter.LowPart;
            adapter.adapterLuidHigh = m_adapter.HighPart;
            return std::unique_ptr<QRhi>(QRhi::create(QRhi::D3D11, &params, {}, &adapter));
        }
#endif
#if defined(Q_OS_MACOS)
        case PatternBackend::Metal: {
            QRhiMetalInitParams params;
            return std::unique_ptr<QRhi>(QRhi::create(QRhi::Metal, &params));
        }
#endif
#if defined(NITPICK_WINDOW_VULKAN)
        case PatternBackend::Vulkan: {
            QRhiVulkanInitParams params;
            params.inst = nitpick::sharedVulkanInstance();
            params.window = this;
            // Unsupported device extensions are ignored; setMetadata checks.
            params.deviceExtensions = {"VK_EXT_hdr_metadata"};
            return std::unique_ptr<QRhi>(QRhi::create(QRhi::Vulkan, &params));
        }
#endif
        default:
            // The constructor rejects a backend that is not built in.
            throw std::runtime_error("unknown QRhi backend");
        }
    }

    // Qt 6.12 behaviour of each backend's createOrResize() for an
    // unsupported format.
    const char *fallbackText() const {
        switch (m_backend) {
        case PatternBackend::D3d11:
            return "D3D11 uses an 8-bit SDR swap chain (sRGB, BT.709); the codes are shown as "
                   "SDR values";
        case PatternBackend::Metal:
            return "Metal keeps the format, but the display has no EDR headroom; macOS clips at "
                   "SDR white";
        default:
            return "Vulkan uses the first surface format of the compositor; the codes are shown "
                   "as SDR values";
        }
    }

    // The format and colour space of the swap chain that QRhi created.
    NativeSwapChain readNativeSwapChain() {
        NativeSwapChain n;
        switch (m_backend) {
#if defined(Q_OS_WIN)
        case PatternBackend::D3d11: {
            ComPtr<IDXGISwapChain1> sc1;
            check(static_cast<QD3D11SwapChain *>(m_sc.get())
                      ->swapChain->QueryInterface(IID_PPV_ARGS(&sc1)),
                  "IDXGISwapChain1");
            DXGI_SWAP_CHAIN_DESC1 desc{};
            check(sc1->GetDesc1(&desc), "GetDesc1");
            DXGI_COLOR_SPACE_TYPE cs = DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709;
            if (desc.Format == DXGI_FORMAT_R10G10B10A2_UNORM) {
                cs = DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
            } else if (desc.Format == DXGI_FORMAT_R16G16B16A16_FLOAT) {
                cs = DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709;
            }
            // DXGI has no call that reads the colour space of a swap chain.
            // Qt sets it at creation, but after a failed HDR creation it sets
            // the HDR colour space on its 8-bit fallback. Setting it again
            // from the actual format makes the recorded tag a fact.
            check(swapChain4()->SetColorSpace1(cs), "SetColorSpace1");
            n.format = dxgiFormatName(desc.Format);
            n.colourSpace = dxgiColourSpaceName(cs);
            n.matchesRequest = desc.Format == (m_fp16 ? DXGI_FORMAT_R16G16B16A16_FLOAT
                                                      : DXGI_FORMAT_R10G10B10A2_UNORM);
            return n;
        }
#endif
#if defined(NITPICK_WINDOW_VULKAN)
        case PatternBackend::Vulkan: {
            const auto *sc = static_cast<QVkSwapChain *>(m_sc.get());
            n.format = vkFormatName(sc->colorFormat);
            n.colourSpace = vkColourSpaceName(sc->colorSpace);
            n.matchesRequest =
                m_fp16 ? sc->colorFormat == VK_FORMAT_R16G16B16A16_SFLOAT &&
                             sc->colorSpace == VK_COLOR_SPACE_EXTENDED_SRGB_LINEAR_EXT
                       : sc->colorFormat == VK_FORMAT_A2B10G10R10_UNORM_PACK32 &&
                             sc->colorSpace == VK_COLOR_SPACE_HDR10_ST2084_EXT;
            return n;
        }
#endif
#if defined(Q_OS_MACOS)
        case PatternBackend::Metal: {
            const nitpick::MetalLayerFormat f = nitpick::metalLayerFormat(this, m_fp16);
            n.format = f.pixelFormat;
            n.colourSpace = f.colourSpace;
            n.matchesRequest = f.matches;
            return n;
        }
#endif
        default:
            throw std::runtime_error("unknown QRhi backend");
        }
    }

    void init() {
        // A second expose after SurfaceAboutToBeDestroyed initializes again.
        // The resources of the earlier QRhi must go before that QRhi.
        m_renderer.reset();
        m_rp.reset();
        m_sc.reset();
        m_rhi = createRhi();
        // Readbacks of the earlier QRhi ended with it.
        m_readbacks.clear();
        if (!m_rhi) {
            throw std::runtime_error("cannot create the QRhi device");
        }
        m_readback = g_probe.readback;
        m_sc.reset(m_rhi->newSwapChain());
        m_sc->setWindow(this);
        if (m_readback) {
            m_sc->setFlags(QRhiSwapChain::UsedAsTransferSource);
        }
        const auto format = m_fp16 ? QRhiSwapChain::HDRExtendedSrgbLinear : QRhiSwapChain::HDR10;
        // QRhi changes an unsupported format without an error. The window
        // still shows the pattern, so that the user can see the remapped
        // output; the launcher shows the fallback as a warning.
        m_fallback = m_sc->isFormatSupported(format) ? "" : fallbackText();
        m_sc->setFormat(format);
        m_rp.reset(m_sc->newCompatibleRenderPassDescriptor());
        m_sc->setRenderPassDescriptor(m_rp.get());
        if (!m_sc->createOrResize()) {
            throw std::runtime_error("cannot create the swap chain");
        }
        m_native = readNativeSwapChain();
        // D3D11 also falls back if the creation of an accepted HDR format
        // fails; only the native format shows it.
        if (!m_native.matchesRequest && m_fallback.empty()) {
            m_fallback = "QRhi created " + m_native.format + " with " + m_native.colourSpace +
                         " instead of the requested swap chain";
        }
        m_renderer = std::make_unique<nitpick::PatternRenderer>(
            m_rhi.get(), m_rp.get(), nitpick::patternTextureFormat(m_fp16));
    }

    QRhi::FrameOpResult beginFrame() {
        if (g_probe.take(FrameFault::BeginDeviceLost)) {
            return QRhi::FrameOpDeviceLost;
        }
        if (g_probe.take(FrameFault::BeginError)) {
            return QRhi::FrameOpError;
        }
        if (g_probe.take(FrameFault::BeginOutOfDate)) {
            return QRhi::FrameOpSwapChainOutOfDate;
        }
        return m_rhi->beginFrame(m_sc.get());
    }

    void render() {
        if (g_probe.hold) {
            return;
        }
        if (m_sc->currentPixelSize() != m_sc->surfacePixelSize()) {
            // The pattern texture keeps its size; the shader draws it 1:1
            // from the top-left corner of the pattern area.
            m_sc->createOrResize();
        }
        QRhi::FrameOpResult r = beginFrame();
        if (r == QRhi::FrameOpSwapChainOutOfDate) {
            m_sc->createOrResize();
            r = beginFrame();
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
        // 0 if no frame is queued, or if the queued frame timed out: its
        // pixels must not appear after the host got the error.
        const std::uint64_t frame = nitpick::frameTakeUpload();
        std::string error;
        if (frame != 0) {
            try {
                if (g_probe.take(FrameFault::Upload)) {
                    throw std::runtime_error("injected upload failure");
                }
                m_renderer->upload(updates, m_pending, m_pendingSize);
                g_probe.uploaded.push_back(frame);
            } catch (const std::exception &e) {
                error = e.what();
            }
        }
        m_pending.clear();
        // The readback of the swap-chain image after the draw, before the
        // present; a failed upload reads back the previous pattern.
        QRhiResourceUpdateBatch *after = nullptr;
        QRhiReadbackResult *readback = nullptr;
        if (frame != 0 && m_readback) {
            // Qt writes the result when the readback completes, which can
            // be after this function if the frame fails; see m_readbacks.
            m_readbacks.push_back(std::make_unique<QRhiReadbackResult>());
            readback = m_readbacks.back().get();
            after = m_rhi->nextResourceUpdateBatch();
            after->readBackTexture(QRhiReadbackDescription(), readback);
        }
        // The safe-area margins can change after the first expose.
        m_renderer->draw(m_sc->currentFrameCommandBuffer(), m_sc->currentFrameRenderTarget(),
                         patternArea().topLeft(), updates, after);
        r = m_rhi->endFrame(m_sc.get());
        if (readback && r == QRhi::FrameOpSuccess) {
            // Completes every pending readback.
            if (m_rhi->finish() == QRhi::FrameOpSuccess) {
                storeReadback(frame, *readback, {});
                m_readbacks.clear();
            } else {
                storeReadback(frame, {}, "QRhi::finish failed");
            }
        }
        if (g_probe.take(FrameFault::EndDeviceLost)) {
            r = QRhi::FrameOpDeviceLost;
        } else if (g_probe.take(FrameFault::EndError)) {
            r = QRhi::FrameOpError;
        }
        if (frame != 0) {
            if (error.empty() && r != QRhi::FrameOpSuccess) {
                error = frameOpError("endFrame", r);
            }
            nitpick::frameEnded(frame, error);
        }
    }

    void storeReadback(std::uint64_t frame, const QRhiReadbackResult &result,
                       const std::string &error) {
        ProbeImage image;
        image.frame = frame;
        image.format = result.format;
        image.data = result.data;
        image.size = result.pixelSize;
        image.yUp = m_rhi->isYUpInFramebuffer();
        image.error = error;
        if (error.empty() && result.data.isEmpty()) {
            // Qt skips a swap-chain readback with only a warning, for example
            // without VK_IMAGE_USAGE_TRANSFER_SRC_BIT or for an unknown format.
            image.error = "QRhi gave no data for the swap chain of format " + m_native.format;
        }
        g_probe.image = std::move(image);
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
    PatternBackend m_backend;
    // Pattern area size of the reference table, see screenPatternSize().
    QSize m_expectedSize;
    bool m_ready = false;
    // The swap chain that QRhi uses instead of the requested format; see init().
    std::string m_fallback;
    NativeSwapChain m_native;
    // The swap chain has UsedAsTransferSource; see g_probe.
    bool m_readback = false;
    std::string m_error;
    std::function<void()> m_onSettled;
    // Pixels of the queued frame; its ID is in the Rust submission state.
    QByteArray m_pending;
    QSize m_pendingSize;
#if defined(Q_OS_WIN)
    LUID m_adapter{};
#endif
    // Readback results that QRhi can still write. Declared before the QRhi,
    // so destroyed after it.
    std::vector<std::unique_ptr<QRhiReadbackResult>> m_readbacks;
    // Declared before the resources, destroyed last: every QRhi resource
    // needs the QRhi.
    std::unique_ptr<QRhi> m_rhi;
    std::unique_ptr<QRhiSwapChain> m_sc;
    std::unique_ptr<QRhiRenderPassDescriptor> m_rp;
    std::unique_ptr<nitpick::PatternRenderer> m_renderer;
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
    id.manufacturer = rust::String(s->manufacturer().toStdString());
    id.model = rust::String(s->model().toStdString());
    id.platform = rust::String(QGuiApplication::platformName().toStdString());
#if defined(Q_OS_WIN)
    id.gdi_name = rust::String(gdiName(s));
#elif defined(Q_OS_MACOS)
    id.display_id = displayIdOf(s);
#endif
    return id;
}

PatternSurface openPatternWindow(std::int32_t screen, bool fp16, PatternBackend backend) {
    QScreen *target = screenAt(screen);
    delete g_window.data();
    g_probe.queued.clear();
    g_probe.uploaded.clear();
    g_probe.image.reset();
    g_probe.metadata.reset();
    auto window = std::make_unique<PatternWindow>(target, fp16, backend);
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

std::uint64_t patternWindowHandle() { return std::uint64_t(openWindow().winId()); }

void presentPattern(rust::Slice<const std::uint8_t> pixels, std::int32_t width,
                    std::int32_t height, std::uint64_t frame, std::int32_t timeoutMs) {
    openWindow().present(pixels.data(), pixels.size(), width, height, frame, timeoutMs);
}

void setDxgiHdrMetadata(const DxgiHdrMetadata &metadata) { openWindow().setMetadata(metadata); }

void setVulkanHdrMetadata(const VulkanHdrMetadata &metadata) {
    openWindow().setMetadata(metadata);
}

void setMetalHdrMetadata(const MetalHdrMetadata &metadata) { openWindow().setMetadata(metadata); }

void clearHdrMetadata() { openWindow().clearMetadata(); }

void setSwapChainReadback(bool enabled) { g_probe.readback = enabled; }

SwapChainRecord swapChainRecord() { return openWindow().record(); }

SwapChainImage takeSwapChainImage() {
    openWindow();
    if (!g_probe.image) {
        throw std::runtime_error("no swap-chain readback since the last call");
    }
    const ProbeImage image = std::move(*g_probe.image);
    g_probe.image.reset();
    SwapChainImage out{};
    out.frame = image.frame;
    out.format = rust::String(textureFormatName(image.format));
    out.data.reserve(std::size_t(image.data.size()));
    for (const char byte : image.data) {
        out.data.push_back(static_cast<std::uint8_t>(byte));
    }
    out.width = image.size.width();
    out.height = image.size.height();
    out.y_up = image.yUp;
    out.error = rust::String(image.error);
    return out;
}

void injectFrameFault(FrameFault fault, std::uint32_t count) {
    g_probe.fault = fault;
    g_probe.faultCount = fault == FrameFault::None ? 0 : count;
}

void holdRenders(bool hold) {
    g_probe.hold = hold;
    if (!hold && g_window) {
        g_window->requestUpdate();
    }
}

rust::Vec<std::uint64_t> queuedFrames() {
    rust::Vec<std::uint64_t> out;
    for (const std::uint64_t f : g_probe.queued) {
        out.push_back(f);
    }
    return out;
}

rust::Vec<std::uint64_t> uploadedFrames() {
    rust::Vec<std::uint64_t> out;
    for (const std::uint64_t f : g_probe.uploaded) {
        out.push_back(f);
    }
    return out;
}

MetadataCall lastMetadataCall() {
    openWindow();
    if (!g_probe.metadata) {
        throw std::runtime_error("no native metadata call since the last open");
    }
    return *g_probe.metadata;
}

void closePatternWindowLater() { openWindow().deleteLater(); }

void runEventLoop(std::int32_t ms) {
    QEventLoop loop;
    QTimer::singleShot(ms, Qt::PreciseTimer, &loop, &QEventLoop::quit);
    loop.exec();
}

} // namespace nitpick
