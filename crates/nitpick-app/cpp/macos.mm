#include "nitpick-app/cpp/macos.h"

#include "nitpick-app/src/display.cxx.h"

#import <AppKit/AppKit.h>

#include <QtGui/QScreen>
#include <QtGui/QWindow>
#include <QtGui/qpa/qplatformwindow_p.h>
#include <QtGui/qscreen_platform.h>

#import <QuartzCore/CAEDRMetadata.h>
#import <QuartzCore/CAMetalLayer.h>

#include <algorithm>
#include <cmath>
#include <stdexcept>

namespace nitpick {

QSize screenSafePixelSize(QScreen *screen) {
    @autoreleasepool {
        auto *native = screen->nativeInterface<QNativeInterface::QCocoaScreen>();
        NSScreen *s = native ? native->nativeScreen() : nil;
        if (!s) {
            throw std::runtime_error("screen has no NSScreen");
        }
        // Points. On a screen with a camera housing, macOS places a full-screen
        // window below the menu bar, which is 1 point taller than the top
        // safe-area inset (MacBook Pro M1 Max: frame 1169, inset 38, menu bar
        // 39, window 1130). The larger value covers a hidden menu bar.
        NSEdgeInsets insets = NSEdgeInsetsZero;
        if (@available(macOS 12.0, *)) {
            insets = s.safeAreaInsets;
        }
        const NSRect frame = s.frame;
        if (insets.top > 0) {
            insets.top = std::max(insets.top, NSMaxY(frame) - NSMaxY(s.visibleFrame));
        }
        const CGFloat scale = s.backingScaleFactor;
        return QSize(int(std::lround((frame.size.width - insets.left - insets.right) * scale)),
                     int(std::lround((frame.size.height - insets.top - insets.bottom) * scale)));
    }
}

std::uint32_t displayIdOf(QScreen *screen) {
    @autoreleasepool {
        auto *native = screen->nativeInterface<QNativeInterface::QCocoaScreen>();
        NSScreen *s = native ? native->nativeScreen() : nil;
        NSNumber *number = s ? s.deviceDescription[@"NSScreenNumber"] : nil;
        if (!number) {
            throw std::runtime_error("screen has no NSScreenNumber");
        }
        return number.unsignedIntValue;
    }
}

void setLayerHdrMetadata(QWindow *window, const MetalHdrMetadata *md) {
    @autoreleasepool {
        // The NSView layer is a container; the CAMetalLayer is its content
        // sublayer. QRhi's Metal swap chain gets the layer the same way.
        auto *cocoa = window->nativeInterface<QNativeInterface::Private::QCocoaWindow>();
        CALayer *content = cocoa ? cocoa->contentLayer() : nil;
        if (![content isKindOfClass:[CAMetalLayer class]]) {
            throw std::runtime_error("the pattern window has no CAMetalLayer");
        }
        CAMetalLayer *layer = (CAMetalLayer *)content;
        if (@available(macOS 10.15, *)) {
            if (!md) {
                layer.EDRMetadata = nil;
                return;
            }
            NSData *display = [NSData dataWithBytes:md->display_info.data()
                                             length:md->display_info.size()];
            NSData *contentInfo = [NSData dataWithBytes:md->content_info.data()
                                                 length:md->content_info.size()];
            layer.EDRMetadata = [CAEDRMetadata HDR10MetadataWithDisplayInfo:display
                                                                contentInfo:contentInfo
                                                         opticalOutputScale:md->optical_output_scale];
        } else {
            throw std::runtime_error("HDR10 EDR metadata needs macOS 10.15 or later");
        }
    }
}

} // namespace nitpick
