//! Windows Graphics Capture of one window into an FP16 frame pool. See
//! "Platform capture adapters" in `doc/design/output_driver/testing.md`.
//!
//! `start` creates the capture item on the host thread and the D3D11 device,
//! frame pool and session on a session thread. The free-threaded frame pool
//! calls the frame handler on a system thread. The handler copies each frame
//! to CPU memory there, so the host thread only takes finished captures.

use std::ffi::c_void;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc as sync_mpsc};
use std::task::{Context, Poll};
use std::thread;

use futures_channel::mpsc;
use futures_core::Stream;
use nitpick_test_host::capture::{
    Capture, CaptureAdapter, CaptureFrames, CaptureTarget, TimeBase, Timestamp,
};
use serde_json::{Map, Value, json};
use windows::Foundation::Metadata::ApiInformation;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R16G16B16A16_FLOAT;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
use windows::core::{HSTRING, IInspectable, Interface, h};

use crate::frame::{RGBA16F_BYTES, WgcFrame};

/// Captures that the handler delivered and the host has not taken. A frame
/// of a 3840×2160 window has 66 MB; the handler drops frames above this
/// count, so memory stays bounded when the host thread is slow.
pub const MAX_QUEUED: usize = 2;

/// Frame pool buffers.
const BUFFERS: i32 = 2;

const PIXEL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::R16G16B16A16Float;

/// Gives the `HWND` of the window that shows the target frame.
pub type Resolve = Box<dyn FnMut(&CaptureTarget<'_>) -> Result<isize, String>>;

/// WGC window capture with an FP16 frame pool.
pub struct WgcAdapter {
    resolve: Resolve,
}

impl WgcAdapter {
    #[must_use]
    pub fn new(resolve: Resolve) -> Self {
        Self { resolve }
    }
}

impl CaptureAdapter for WgcAdapter {
    fn now(&self) -> Option<Timestamp> {
        qpc_now()
    }

    fn start(&mut self, target: &CaptureTarget<'_>) -> Result<CaptureFrames, String> {
        let window = (self.resolve)(target)?;
        let item = create_item(window)?;
        let (tx, rx) = mpsc::unbounded();
        let queued = Arc::new(AtomicUsize::new(0));
        let (stop, stopped) = sync_mpsc::channel::<()>();
        let handler_queued = Arc::clone(&queued);
        thread::Builder::new()
            .name("nitpick-wgc".into())
            .spawn(
                move || match Running::start(item, window, &tx, handler_queued) {
                    // Until the stream drops its sender.
                    Ok(session) => {
                        let _ = stopped.recv();
                        drop(session);
                    }
                    Err(e) => {
                        let _ = tx.unbounded_send(Err(e));
                    }
                },
            )
            .map_err(|e| format!("cannot start the capture thread: {e}"))?;
        Ok(Box::pin(Frames {
            rx,
            queued,
            _stop: stop,
        }))
    }
}

/// The frames of one session. Dropping it ends the session.
struct Frames {
    rx: mpsc::UnboundedReceiver<Result<Capture, String>>,
    queued: Arc<AtomicUsize>,
    _stop: sync_mpsc::Sender<()>,
}

impl Stream for Frames {
    type Item = Result<Capture, String>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let item = Pin::new(&mut self.rx).poll_next(cx);
        if let Poll::Ready(Some(Ok(_))) = &item {
            self.queued.fetch_sub(1, Ordering::AcqRel);
        }
        item
    }
}

/// `QueryPerformanceCounter` at its own tick rate.
#[must_use]
pub fn qpc_now() -> Option<Timestamp> {
    let (mut frequency, mut counter) = (0i64, 0i64);
    // SAFETY: both pointers are to live locals.
    unsafe {
        QueryPerformanceFrequency(&raw mut frequency).ok()?;
        QueryPerformanceCounter(&raw mut counter).ok()?;
    }
    Some(Timestamp {
        base: TimeBase::Qpc,
        ticks: u64::try_from(counter).ok()?,
        ticks_per_second: u64::try_from(frequency).ok()?,
    })
}

fn api(what: &'static str) -> impl Fn(windows::core::Error) -> String {
    move |e| format!("{what} failed: {e}")
}

fn hwnd(window: isize) -> HWND {
    HWND(window as *mut c_void)
}

/// # Errors
/// WGC is not supported, or the window has no capture item, for example
/// because it does not exist.
fn create_item(window: isize) -> Result<GraphicsCaptureItem, String> {
    if !GraphicsCaptureSession::IsSupported().map_err(api("GraphicsCaptureSession::IsSupported"))? {
        return Err("Windows Graphics Capture is not supported on this system".into());
    }
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
        .map_err(api("IGraphicsCaptureItemInterop"))?;
    // SAFETY: the interop checks the handle; an invalid one gives an error.
    unsafe { interop.CreateForWindow(hwnd(window)) }
        .map_err(|e| format!("CreateForWindow({window:#x}) failed: {e}"))
}

fn session_property(name: &str) -> bool {
    ApiInformation::IsPropertyPresent(
        h!("Windows.Graphics.Capture.GraphicsCaptureSession"),
        &HSTRING::from(name),
    )
    .unwrap_or(false)
}

/// A hardware D3D11 device on the default adapter, and the adapter name.
fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext, String), String> {
    let (mut device, mut context) = (None, None);
    // SAFETY: the out pointers are to live locals.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&raw mut device),
            None,
            Some(&raw mut context),
        )
    }
    .map_err(api("D3D11CreateDevice"))?;
    let (Some(device), Some(context)) = (device, context) else {
        return Err("D3D11CreateDevice gave no device".into());
    };
    let dxgi: IDXGIDevice = device.cast().map_err(api("IDXGIDevice"))?;
    // SAFETY: plain COM calls on a live device.
    let desc = unsafe { dxgi.GetAdapter().and_then(|a| a.GetDesc()) }
        .map_err(api("IDXGIAdapter::GetDesc"))?;
    let len = desc
        .Description
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(desc.Description.len());
    let name = String::from_utf16_lossy(&desc.Description[..len]);
    Ok((device, context, name))
}

fn winrt_device(device: &ID3D11Device) -> Result<IDirect3DDevice, String> {
    let dxgi: IDXGIDevice = device.cast().map_err(api("IDXGIDevice"))?;
    // SAFETY: `dxgi` is a live DXGI device.
    let inspectable: IInspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
        .map_err(api("CreateDirect3D11DeviceFromDXGIDevice"))?;
    inspectable.cast().map_err(api("IDirect3DDevice"))
}

/// The process of the window; 0 if it has none.
fn window_process(window: isize) -> u32 {
    let mut process = 0u32;
    // SAFETY: the out pointer is to a live local; an invalid handle gives 0.
    unsafe { GetWindowThreadProcessId(hwnd(window), Some(&raw mut process)) };
    process
}

/// The objects of a running capture. Dropping it ends the capture.
struct Running {
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    capture: GraphicsCaptureSession,
    frame_token: i64,
    closed_token: i64,
}

impl Running {
    fn start(
        item: GraphicsCaptureItem,
        window: isize,
        tx: &mpsc::UnboundedSender<Result<Capture, String>>,
        queued: Arc<AtomicUsize>,
    ) -> Result<Self, String> {
        let (device, context, adapter) = create_device()?;
        let size = item.Size().map_err(api("GraphicsCaptureItem::Size"))?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device(&device)?,
            PIXEL_FORMAT,
            BUFFERS,
            size,
        )
        .map_err(api("Direct3D11CaptureFramePool::CreateFreeThreaded"))?;
        let capture = pool
            .CreateCaptureSession(&item)
            .map_err(api("CreateCaptureSession"))?;
        // Without this property the API draws the cursor at an unknown
        // position; frame acceptance then rejects each frame.
        let cursor_hidden = session_property("IsCursorCaptureEnabled");
        if cursor_hidden {
            capture
                .SetIsCursorCaptureEnabled(false)
                .map_err(api("SetIsCursorCaptureEnabled"))?;
        }
        let config = config(cursor_hidden, border_off(&capture), &adapter);
        let mut identity = Map::new();
        identity.insert(
            "display_name".into(),
            json!(item.DisplayName().map(|n| n.to_string()).ok()),
        );
        identity.insert("window_process".into(), json!(window_process(window)));

        let failed = Arc::new(AtomicBool::new(false));
        let closed_tx = tx.clone();
        let closed_failed = Arc::clone(&failed);
        let closed_token = item
            .Closed(
                &TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(move |_, _| {
                    if !closed_failed.swap(true, Ordering::AcqRel) {
                        let _ = closed_tx.unbounded_send(Err("the captured window closed".into()));
                    }
                    Ok(())
                }),
            )
            .map_err(api("GraphicsCaptureItem::Closed"))?;
        let handler = Mutex::new(Handler {
            device,
            context,
            staging: None,
            size,
            window,
            cursor_hidden,
            identity,
            config,
            dropped: 0,
            queued,
        });
        let frame_token = pool
            .FrameArrived(&frame_handler(handler, failed, tx.clone()))
            .map_err(api("Direct3D11CaptureFramePool::FrameArrived"))?;
        let running = Self {
            item,
            pool,
            capture,
            frame_token,
            closed_token,
        };
        running
            .capture
            .StartCapture()
            .map_err(api("StartCapture"))?;
        Ok(running)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.pool.RemoveFrameArrived(self.frame_token);
        let _ = self.item.RemoveClosed(self.closed_token);
        let _ = self.capture.Close();
        let _ = self.pool.Close();
    }
}

fn config(cursor_hidden: bool, border: Value, adapter: &str) -> Map<String, Value> {
    let mut config = Map::new();
    config.insert("api".into(), json!("Windows.Graphics.Capture"));
    config.insert(
        "item".into(),
        json!("IGraphicsCaptureItemInterop::CreateForWindow"),
    );
    config.insert("pixel_format".into(), json!("R16G16B16A16Float"));
    config.insert("frame_pool".into(), json!("CreateFreeThreaded"));
    config.insert("buffers".into(), json!(BUFFERS));
    config.insert("max_queued".into(), json!(MAX_QUEUED));
    config.insert("cursor_capture".into(), json!(!cursor_hidden));
    config.insert("border".into(), border);
    config.insert("adapter".into(), json!(adapter));
    config
}

/// Delivers each frame of the pool, or the first error, through `tx`.
fn frame_handler(
    handler: Mutex<Handler>,
    failed: Arc<AtomicBool>,
    tx: mpsc::UnboundedSender<Result<Capture, String>>,
) -> TypedEventHandler<Direct3D11CaptureFramePool, IInspectable> {
    TypedEventHandler::new(
        move |pool: windows::core::Ref<Direct3D11CaptureFramePool>, _| {
            let Some(pool) = pool.as_ref() else {
                return Ok(());
            };
            if failed.load(Ordering::Acquire) {
                return Ok(());
            }
            let mut handler = handler.lock().unwrap_or_else(PoisonError::into_inner);
            match handler.next(pool) {
                Ok(None) => {}
                Ok(Some(capture)) => {
                    handler.queued.fetch_add(1, Ordering::AcqRel);
                    if tx.unbounded_send(Ok(capture)).is_err() {
                        handler.queued.fetch_sub(1, Ordering::AcqRel);
                    }
                }
                Err(e) => {
                    if !failed.swap(true, Ordering::AcqRel) {
                        let _ = tx.unbounded_send(Err(e));
                    }
                }
            }
            Ok(())
        },
    )
}

/// Asks for a capture without the yellow border and records the result.
/// The border is drawn on the screen; whether it is in the captured image
/// is checked by the capture test, not here.
fn border_off(session: &GraphicsCaptureSession) -> Value {
    if !session_property("IsBorderRequired") {
        return json!({ "set": null, "required": null });
    }
    let set = session
        .SetIsBorderRequired(false)
        .map_err(|e| e.to_string());
    json!({
        "set": set.err(),
        "required": session.IsBorderRequired().ok(),
    })
}

/// The state of the frame handler.
struct Handler {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    /// CPU-readable copy target of the last texture description.
    staging: Option<(D3D11_TEXTURE2D_DESC, ID3D11Texture2D)>,
    /// The frame pool size.
    size: SizeInt32,
    window: isize,
    cursor_hidden: bool,
    identity: Map<String, Value>,
    config: Map<String, Value>,
    /// Frames dropped since the last delivered frame.
    dropped: u64,
    queued: Arc<AtomicUsize>,
}

impl Handler {
    /// The next frame of the pool as a capture. `None`: no frame, or the
    /// host has [`MAX_QUEUED`] captures that it has not taken.
    fn next(&mut self, pool: &Direct3D11CaptureFramePool) -> Result<Option<Capture>, String> {
        let frame = match pool.TryGetNextFrame() {
            Ok(frame) => frame,
            // A null frame: no frame is ready.
            Err(e) if e.code().is_ok() => return Ok(None),
            Err(e) => return Err(format!("TryGetNextFrame failed: {e}")),
        };
        let arrival = qpc_now().ok_or("QueryPerformanceCounter failed")?;
        let content = frame
            .ContentSize()
            .map_err(api("Direct3D11CaptureFrame::ContentSize"))?;
        let result = if self.queued.load(Ordering::Acquire) >= MAX_QUEUED {
            self.dropped += 1;
            None
        } else {
            Some(self.copy(&frame, content, arrival)?)
        };
        let _ = frame.Close();
        // The texture of each frame has the pool size. After a resize the
        // content is cut or padded until the pool has the new size.
        if content != self.size && content.Width > 0 && content.Height > 0 {
            pool.Recreate(&winrt_device(&self.device)?, PIXEL_FORMAT, BUFFERS, content)
                .map_err(api("Direct3D11CaptureFramePool::Recreate"))?;
            self.size = content;
        }
        Ok(result)
    }

    fn copy(
        &mut self,
        frame: &Direct3D11CaptureFrame,
        content: SizeInt32,
        arrival: Timestamp,
    ) -> Result<Capture, String> {
        let time = frame
            .SystemRelativeTime()
            .map_err(api("Direct3D11CaptureFrame::SystemRelativeTime"))?;
        let access: IDirect3DDxgiInterfaceAccess = frame
            .Surface()
            .and_then(|s| s.cast())
            .map_err(api("Direct3D11CaptureFrame::Surface"))?;
        // SAFETY: the surface of a D3D11 frame pool is a 2D texture.
        let texture: ID3D11Texture2D =
            unsafe { access.GetInterface() }.map_err(api("IDirect3DDxgiInterfaceAccess"))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: the out pointer is to a live local.
        unsafe { texture.GetDesc(&raw mut desc) };
        if desc.Format != DXGI_FORMAT_R16G16B16A16_FLOAT {
            return Err(format!(
                "the frame texture has DXGI format {}, not R16G16B16A16_FLOAT",
                desc.Format.0
            ));
        }
        let width = usize::try_from(desc.Width).map_err(|e| e.to_string())?;
        let height = usize::try_from(desc.Height).map_err(|e| e.to_string())?;
        let staging = self.staging(&desc)?;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: both textures have the same description; Map gives a
        // pointer to `RowPitch × Height` bytes until Unmap.
        let (row_pitch, bytes) = unsafe {
            self.context.CopyResource(&staging, &texture);
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&raw mut mapped))
                .map_err(api("ID3D11DeviceContext::Map"))?;
            let row_pitch = mapped.RowPitch as usize;
            let len = row_pitch * (height - 1) + width * RGBA16F_BYTES;
            let bytes = std::slice::from_raw_parts(mapped.pData.cast::<u8>(), len).to_vec();
            self.context.Unmap(&staging, 0);
            (row_pitch, bytes)
        };
        let mut raw = self.identity.clone();
        raw.insert(
            "texture".into(),
            json!({
                "format": desc.Format.0,
                "width": desc.Width,
                "height": desc.Height,
                "mip_levels": desc.MipLevels,
                "array_size": desc.ArraySize,
                "sample_count": desc.SampleDesc.Count,
                "sample_quality": desc.SampleDesc.Quality,
                "usage": desc.Usage.0,
                "bind_flags": desc.BindFlags,
                "cpu_access_flags": desc.CPUAccessFlags,
                "misc_flags": desc.MiscFlags,
            }),
        );
        raw.insert("dropped_before".into(), json!(self.dropped));
        self.dropped = 0;
        Ok(WgcFrame {
            width,
            height,
            row_pitch,
            bytes,
            content_size: (content.Width, content.Height),
            system_relative_time: time.Duration,
            arrival,
            window: self.window,
            cursor_hidden: self.cursor_hidden,
            raw,
            config: self.config.clone(),
        }
        .into_capture())
    }

    /// A staging texture with the size and format of `desc`.
    fn staging(&mut self, desc: &D3D11_TEXTURE2D_DESC) -> Result<ID3D11Texture2D, String> {
        if let Some((d, texture)) = &self.staging
            && (d.Width, d.Height, d.Format) == (desc.Width, desc.Height, desc.Format)
        {
            return Ok(texture.clone());
        }
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: desc.Width,
            Height: desc.Height,
            MipLevels: 1,
            ArraySize: 1,
            Format: desc.Format,
            SampleDesc: desc.SampleDesc,
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut texture = None;
        // SAFETY: the out pointer is to a live local.
        unsafe {
            self.device
                .CreateTexture2D(&raw const staging_desc, None, Some(&raw mut texture))
        }
        .map_err(api("CreateTexture2D"))?;
        let texture = texture.ok_or("CreateTexture2D gave no texture")?;
        self.staging = Some((*desc, texture.clone()));
        Ok(texture)
    }
}

#[cfg(test)]
#[path = "wgc_tests.rs"]
mod tests;
