#include "nitpick-app/cpp/pattern_window.h"

#include <QtCore/QPointer>
#include <QtGui/QGuiApplication>
#include <QtGui/QKeyEvent>
#include <QtGui/QScreen>
#include <QtGui/QWindow>

#include <cstdio>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>

#ifdef _WIN32

#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>

#include <QtGui/qscreen_platform.h>
#include <d3d11.h>
#include <dxgi1_6.h>
#include <wrl/client.h>

using Microsoft::WRL::ComPtr;

namespace {

void check(HRESULT hr, const char *what) {
    if (FAILED(hr)) {
        char buf[16];
        std::snprintf(buf, sizeof buf, "0x%08lX", static_cast<unsigned long>(hr));
        throw std::runtime_error(std::string(what) + " failed: " + buf);
    }
}

QScreen *screenAt(std::int32_t index) {
    const auto screens = QGuiApplication::screens();
    if (index < 0 || index >= screens.size()) {
        throw std::runtime_error("no screen with index " + std::to_string(index));
    }
    return screens.at(index);
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
void findOutput(IDXGIFactory2 *factory, HMONITOR monitor, ComPtr<IDXGIAdapter1> &adapterOut,
                ComPtr<IDXGIOutput6> &outputOut) {
    ComPtr<IDXGIAdapter1> adapter;
    for (UINT a = 0; factory->EnumAdapters1(a, &adapter) != DXGI_ERROR_NOT_FOUND; ++a) {
        ComPtr<IDXGIOutput> output;
        for (UINT o = 0; adapter->EnumOutputs(o, &output) != DXGI_ERROR_NOT_FOUND; ++o) {
            DXGI_OUTPUT_DESC desc{};
            if (SUCCEEDED(output->GetDesc(&desc)) && desc.Monitor == monitor &&
                SUCCEEDED(output.As(&outputOut))) {
                adapterOut = adapter;
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
    char buf[512];
    std::snprintf(buf, sizeof buf,
                  "%ls: %ldx%ld, %u bpc, Windows HDR %s, EDID peak %.0f cd/m2, min %.4f cd/m2",
                  d.DeviceName, d.DesktopCoordinates.right - d.DesktopCoordinates.left,
                  d.DesktopCoordinates.bottom - d.DesktopCoordinates.top, d.BitsPerColor,
                  hdr ? "on" : "OFF (signal is converted to SDR)", d.MaxLuminance,
                  d.MinLuminance);
    return buf;
}

// Borderless full-screen window that scans out one static 10-bit image.
//
// The image goes to the back buffer with CopyResource, not a draw call, so
// no sampling, blending or colour conversion can change a code value inside
// the application. The swap chain colour space is PQ / BT.2020
// (DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020); DWM and the driver may still
// alter values after Present, which is what the external scope verifies.
class PatternWindow final : public QWindow {
public:
    PatternWindow(QScreen *screen, const std::uint32_t *pixels, int width, int height) {
        setSurfaceType(QSurface::Direct3DSurface);
        setFlags(Qt::Window | Qt::FramelessWindowHint);
        setScreen(screen);
        setGeometry(screen->geometry());
        setCursor(Qt::BlankCursor);
        setTitle(QStringLiteral("nitpick pattern"));

        ComPtr<IDXGIFactory2> factory;
        check(CreateDXGIFactory2(0, IID_PPV_ARGS(&factory)), "CreateDXGIFactory2");
        ComPtr<IDXGIAdapter1> adapter;
        findOutput(factory.Get(), monitorOf(screen), adapter, m_output);

        check(D3D11CreateDevice(adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0, nullptr, 0,
                                D3D11_SDK_VERSION, &m_device, nullptr, &m_context),
              "D3D11CreateDevice");

        D3D11_TEXTURE2D_DESC td{};
        td.Width = UINT(width);
        td.Height = UINT(height);
        td.MipLevels = 1;
        td.ArraySize = 1;
        td.Format = DXGI_FORMAT_R10G10B10A2_UNORM;
        td.SampleDesc.Count = 1;
        td.Usage = D3D11_USAGE_IMMUTABLE;
        td.BindFlags = D3D11_BIND_SHADER_RESOURCE;
        D3D11_SUBRESOURCE_DATA init{pixels, UINT(width) * 4, 0};
        check(m_device->CreateTexture2D(&td, &init, &m_source), "CreateTexture2D");

        create();
        const auto hwnd = reinterpret_cast<HWND>(winId());

        DXGI_SWAP_CHAIN_DESC1 sd{};
        sd.Width = UINT(width);
        sd.Height = UINT(height);
        sd.Format = DXGI_FORMAT_R10G10B10A2_UNORM;
        sd.SampleDesc.Count = 1;
        sd.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
        sd.BufferCount = 2;
        // No scaling: a resampled pixel would not keep its code value.
        sd.Scaling = DXGI_SCALING_NONE;
        sd.SwapEffect = DXGI_SWAP_EFFECT_FLIP_DISCARD;
        sd.AlphaMode = DXGI_ALPHA_MODE_IGNORE;
        ComPtr<IDXGISwapChain1> sc1;
        check(factory->CreateSwapChainForHwnd(m_device.Get(), hwnd, &sd, nullptr, nullptr, &sc1),
              "CreateSwapChainForHwnd");
        check(factory->MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER),
              "MakeWindowAssociation");
        check(sc1.As(&m_swapChain), "IDXGISwapChain3");

        constexpr auto pq2020 = DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
        UINT support = 0;
        check(m_swapChain->CheckColorSpaceSupport(pq2020, &support), "CheckColorSpaceSupport");
        if (!(support & DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT)) {
            throw std::runtime_error("swap chain does not support PQ / BT.2020 output");
        }
        check(m_swapChain->SetColorSpace1(pq2020), "SetColorSpace1");
    }

    std::string describe() const { return describeOutput(m_output.Get()); }

protected:
    void exposeEvent(QExposeEvent *) override {
        if (isExposed()) {
            present();
        }
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
    // Flip-model buffers are discarded after Present, so every frame copies
    // the source again.
    void present() {
        ComPtr<ID3D11Texture2D> back;
        if (FAILED(m_swapChain->GetBuffer(0, IID_PPV_ARGS(&back)))) {
            return;
        }
        m_context->CopyResource(back.Get(), m_source.Get());
        m_swapChain->Present(1, 0);
    }

    ComPtr<ID3D11Device> m_device;
    ComPtr<ID3D11DeviceContext> m_context;
    ComPtr<ID3D11Texture2D> m_source;
    ComPtr<IDXGISwapChain3> m_swapChain;
    ComPtr<IDXGIOutput6> m_output;
};

QPointer<PatternWindow> g_window;

} // namespace

namespace nitpick {

std::int32_t screenPixelWidth(std::int32_t screen) {
    const RECT r = monitorRect(screenAt(screen));
    return r.right - r.left;
}

std::int32_t screenPixelHeight(std::int32_t screen) {
    const RECT r = monitorRect(screenAt(screen));
    return r.bottom - r.top;
}

rust::String showPattern(rust::Slice<const std::uint32_t> pixels, std::int32_t width,
                         std::int32_t height, std::int32_t screen) {
    if (width <= 0 || height <= 0 ||
        pixels.size() != std::size_t(width) * std::size_t(height)) {
        throw std::runtime_error("pixel buffer does not match the image size");
    }
    QScreen *target = screenAt(screen);
    delete g_window.data();
    auto window = std::make_unique<PatternWindow>(target, pixels.data(), width, height);
    window->showFullScreen();
    window->requestActivate();
    std::string status = window->describe();
    const RECT r = monitorRect(target);
    if (r.right - r.left != width || r.bottom - r.top != height) {
        status += "; WARNING: image size differs from the native screen size";
    }
    g_window = window.release();
    return rust::String(status);
}

} // namespace nitpick

#else // !_WIN32

namespace nitpick {

[[noreturn]] static void unsupported() {
    throw std::runtime_error("the HDR pattern window supports Windows only");
}

std::int32_t screenPixelWidth(std::int32_t) { unsupported(); }
std::int32_t screenPixelHeight(std::int32_t) { unsupported(); }
rust::String showPattern(rust::Slice<const std::uint32_t>, std::int32_t, std::int32_t,
                         std::int32_t) {
    unsupported();
}

} // namespace nitpick

#endif
