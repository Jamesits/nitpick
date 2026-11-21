//! Test probe of the pattern window: the native swap chain record, the
//! swap-chain readback, render faults and render hold. Only tests use it.
//!
//! All calls are on the Qt GUI thread. The probe state is global and
//! applies to the open pattern window; an open clears the logs.

#[cxx_qt::bridge]
mod ffi {
    /// A render step that gives a fault instead of its result.
    #[namespace = "nitpick"]
    #[repr(u8)]
    #[derive(Debug)]
    enum FrameFault {
        /// Clears the injected fault.
        None,
        /// `beginFrame` gives `FrameOpDeviceLost` and is not called.
        BeginDeviceLost,
        /// `beginFrame` gives `FrameOpError` and is not called.
        BeginError,
        /// `beginFrame` gives `FrameOpSwapChainOutOfDate` and is not called.
        BeginOutOfDate,
        /// `endFrame` runs; its result is replaced by `FrameOpDeviceLost`.
        EndDeviceLost,
        /// `endFrame` runs; its result is replaced by `FrameOpError`.
        EndError,
        /// The texture upload of the queued frame fails.
        Upload,
    }

    /// The swap chain of the open pattern window.
    #[namespace = "nitpick"]
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct SwapChainRecord {
        /// `QRhi::backendName`.
        backend: String,
        /// Native format name, for example `DXGI_FORMAT_R10G10B10A2_UNORM`.
        native_format: String,
        /// Native colour space name, for example
        /// `VK_COLOR_SPACE_HDR10_ST2084_EXT`.
        colour_space: String,
        /// The native format and colour space are those of the request.
        matches_request: bool,
        /// See `PatternSurface::fallback`.
        fallback: String,
        /// `QRhiSwapChain::currentPixelSize`.
        width: i32,
        height: i32,
        /// The pattern area in the swap chain, in pixels.
        area_x: i32,
        area_y: i32,
        area_width: i32,
        area_height: i32,
        /// The swap chain has `UsedAsTransferSource`.
        readback: bool,
    }

    /// The readback of the swap-chain image after the draw of `frame`.
    #[namespace = "nitpick"]
    struct SwapChainImage {
        frame: u64,
        /// `QRhiTexture::Format` of the readback, for example `RGB10A2`.
        format: String,
        /// `QRhiReadbackResult::data`, unchanged.
        data: Vec<u8>,
        width: i32,
        height: i32,
        /// `QRhi::isYUpInFramebuffer`.
        y_up: bool,
        /// Empty if the readback completed.
        error: String,
    }

    /// One native metadata call of the window.
    #[namespace = "nitpick"]
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct MetadataCall {
        /// For example `SetHDRMetaData(HDR10)`.
        api: String,
        /// The bytes of the native structure as passed to the call:
        /// `DXGI_HDR_METADATA_HDR10`; `VkHdrMetadataEXT` from
        /// `displayPrimaryRed` to `maxFrameAverageLightLevel`; the two
        /// `CAEDRMetadata` payloads and the `f32` scale.
        data: Vec<u8>,
        /// Empty if the call succeeded.
        error: String,
    }

    unsafe extern "C++" {
        include!("nitpick-app/cpp/swap_chain_probe.h");

        #[namespace = "nitpick"]
        #[cxx_name = "setSwapChainReadback"]
        fn set_swap_chain_readback(enabled: bool);
        #[namespace = "nitpick"]
        #[cxx_name = "swapChainRecord"]
        fn swap_chain_record() -> Result<SwapChainRecord>;
        #[namespace = "nitpick"]
        #[cxx_name = "takeSwapChainImage"]
        fn take_swap_chain_image() -> Result<SwapChainImage>;
        #[namespace = "nitpick"]
        #[cxx_name = "injectFrameFault"]
        fn inject_frame_fault(fault: FrameFault, count: u32);
        #[namespace = "nitpick"]
        #[cxx_name = "holdRenders"]
        fn hold_renders(hold: bool);
        #[namespace = "nitpick"]
        #[cxx_name = "queuedFrames"]
        fn queued_frames() -> Vec<u64>;
        #[namespace = "nitpick"]
        #[cxx_name = "uploadedFrames"]
        fn uploaded_frames() -> Vec<u64>;
        #[namespace = "nitpick"]
        #[cxx_name = "lastMetadataCall"]
        fn last_metadata_call() -> Result<MetadataCall>;
        #[namespace = "nitpick"]
        #[cxx_name = "closePatternWindowLater"]
        fn close_pattern_window_later() -> Result<()>;
        #[namespace = "nitpick"]
        #[cxx_name = "runEventLoop"]
        fn run_event_loop(ms: i32);
    }
}

pub use ffi::{FrameFault, MetadataCall, SwapChainRecord};

/// The readback of the swap-chain image after the completed draw of `frame`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapChainImage {
    pub frame: u64,
    /// `QRhiTexture::Format` of the readback, for example `RGB10A2`.
    pub format: String,
    /// Tightly packed rows from the top-left pixel.
    pub data: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

/// From the next open, the window reads back each swap-chain image that
/// holds an uploaded frame. The swap chain then has `UsedAsTransferSource`.
pub fn set_swap_chain_readback(enabled: bool) {
    ffi::set_swap_chain_readback(enabled);
}

/// # Errors
/// No pattern window is open.
pub fn swap_chain_record() -> Result<SwapChainRecord, String> {
    ffi::swap_chain_record().map_err(|e| e.what().to_owned())
}

/// The readback of the last render that uploaded a frame, once.
///
/// # Errors
/// No window or no readback, the backend gave no readback (for example
/// without swap-chain readback support), or its layout is invalid.
pub fn take_swap_chain_image(bytes_per_pixel: usize) -> Result<SwapChainImage, String> {
    let raw = ffi::take_swap_chain_image().map_err(|e| e.what().to_owned())?;
    if !raw.error.is_empty() {
        return Err(format!(
            "swap-chain readback of frame {} unavailable: {}",
            raw.frame, raw.error
        ));
    }
    let size = |v: i32| usize::try_from(v).map_err(|_| format!("invalid readback size {v}"));
    let (width, height) = (size(raw.width)?, size(raw.height)?);
    let data = crate::readback::normalize(raw.data, (width, height), bytes_per_pixel, raw.y_up)?;
    Ok(SwapChainImage {
        frame: raw.frame,
        format: raw.format,
        data,
        width,
        height,
    })
}

/// The next `count` calls of the render step of `fault` give the fault.
/// [`FrameFault::None`] clears it.
pub fn inject_frame_fault(fault: FrameFault, count: u32) {
    ffi::inject_frame_fault(fault, count);
}

/// While held, renders do nothing, as when the window gets no update.
pub fn hold_renders(hold: bool) {
    ffi::hold_renders(hold);
}

/// Frame IDs queued in the window since the last open.
#[must_use]
pub fn queued_frames() -> Vec<u64> {
    ffi::queued_frames()
}

/// Frame IDs uploaded without an error since the last open.
#[must_use]
pub fn uploaded_frames() -> Vec<u64> {
    ffi::uploaded_frames()
}

/// # Errors
/// No window, or no native metadata call since the last open.
pub fn last_metadata_call() -> Result<MetadataCall, String> {
    ffi::last_metadata_call().map_err(|e| e.what().to_owned())
}

/// Closes the window from its event loop, as Esc does.
///
/// # Errors
/// No pattern window is open.
pub fn close_pattern_window_later() -> Result<(), String> {
    ffi::close_pattern_window_later().map_err(|e| e.what().to_owned())
}

/// Runs the Qt event loop for `ms` milliseconds.
pub fn run_event_loop(ms: i32) {
    ffi::run_event_loop(ms);
}
