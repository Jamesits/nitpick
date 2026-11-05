#include "nitpick-app/cpp/metal_hdr.h"

#include "nitpick-app/src/display.cxx.h"

#include <QtGui/QWindow>

#import <AppKit/AppKit.h>
#import <QuartzCore/CAEDRMetadata.h>
#import <QuartzCore/CAMetalLayer.h>

#include <stdexcept>

namespace nitpick {

void setMetalHdrMetadata(QWindow *window, const HdrMetadata &md) {
    @autoreleasepool {
        // A MetalSurface QWindow is backed by an NSView with a CAMetalLayer.
        NSView *view = reinterpret_cast<NSView *>(window->winId());
        if (![view.layer isKindOfClass:[CAMetalLayer class]]) {
            throw std::runtime_error("the pattern window has no CAMetalLayer");
        }
        CAMetalLayer *layer = (CAMetalLayer *)view.layer;
        if (@available(macOS 10.15, *)) {
            NSData *display = [NSData dataWithBytes:md.mastering_display.data()
                                             length:md.mastering_display.size()];
            NSData *content = [NSData dataWithBytes:md.content_light_level.data()
                                             length:md.content_light_level.size()];
            // 100: linear 1.0 is 100 cd/m², the EDR convention (as in Chromium).
            layer.EDRMetadata = [CAEDRMetadata HDR10MetadataWithDisplayInfo:display
                                                                contentInfo:content
                                                         opticalOutputScale:100.0f];
        } else {
            throw std::runtime_error("HDR10 EDR metadata needs macOS 10.15 or later");
        }
    }
}

} // namespace nitpick
