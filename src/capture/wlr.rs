use crate::capture::{CaptureBackend, DamageRegion};
use crate::convert::convert_bgrx_to_rgba_inplace;
use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::shm::ShmBuffer;

use std::os::fd::{AsFd, AsRawFd, OwnedFd, FromRawFd};
use std::sync::{atomic::{AtomicBool, Ordering}, mpsc, Arc};
use std::time::Duration;

use wayland_client::{
    protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool},
    Connection, Dispatch, QueueHandle, WEnum,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_dmabuf_v1::{self, ZwpLinuxDmabufV1},
    zwp_linux_buffer_params_v1::{self, ZwpLinuxBufferParamsV1},
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
};

// DRM fourcc & modifier constants
const DRM_FORMAT_XRGB8888: u32 = 0x3432_5258;
const DRM_FORMAT_ARGB8888: u32 = 0x3432_5241;
const DRM_FORMAT_XBGR8888: u32 = 0x3432_4258;
const DRM_FORMAT_ABGR8888: u32 = 0x3432_4241;

// ─── libgbm FFI ────────────────────────────────────────────────────────────

#[allow(non_camel_case_types)]
mod gbm_ffi {
    use libc::c_int;

    pub enum GbmDevice {}
    pub enum GbmBo {}

    pub const GBM_BO_USE_RENDERING: u32 = 1 << 2;
    pub const GBM_BO_USE_LINEAR:    u32 = 1 << 4;

    #[link(name = "gbm")]
    extern "C" {
        pub fn gbm_create_device(fd: c_int) -> *mut GbmDevice;
        pub fn gbm_device_destroy(gbm: *mut GbmDevice);
        pub fn gbm_bo_create(
            gbm: *mut GbmDevice,
            width: u32,
            height: u32,
            format: u32,
            flags: u32,
        ) -> *mut GbmBo;
        pub fn gbm_bo_destroy(bo: *mut GbmBo);
        pub fn gbm_bo_get_fd(bo: *mut GbmBo) -> c_int;
        pub fn gbm_bo_get_stride(bo: *mut GbmBo) -> u32;
    }
}

struct GbmDevice {
    ptr: *mut gbm_ffi::GbmDevice,
    // gbm_create_device() stores this fd internally without dup'ing it, so the
    // File must stay open for as long as the device is alive.
    _file: std::fs::File,
}

impl Drop for GbmDevice {
    fn drop(&mut self) {
        unsafe { gbm_ffi::gbm_device_destroy(self.ptr) };
    }
}

impl GbmDevice {
    fn try_open(path: &str) -> Option<Self> {
        let file = std::fs::OpenOptions::new().read(true).write(true).open(path).ok()?;
        let ptr = unsafe { gbm_ffi::gbm_create_device(file.as_raw_fd()) };
        if ptr.is_null() { return None; }
        Some(GbmDevice { ptr, _file: file })
    }
}

unsafe impl Send for GbmDevice {}

// ─── Wayland state ─────────────────────────────────────────────────────────

struct WlrState {
    shm: Option<wl_shm::WlShm>,
    output: Option<wl_output::WlOutput>,
    screencopy_manager: Option<ZwlrScreencopyManagerV1>,
    linux_dmabuf: Option<ZwpLinuxDmabufV1>,
    supported_dma: Vec<(u32, u64)>, // (drm_format, modifier)
    frame_info: FrameInfo,
}

#[derive(Default)]
struct FrameInfo {
    shm_width: u32, shm_height: u32, shm_stride: u32,
    shm_format: Option<wl_shm::Format>,
    got_shm: bool,
    dma_format: u32, dma_width: u32, dma_height: u32,
    got_dma: bool,
    buffer_done: bool,
    ready: bool,
    failed: bool,
    damage: Vec<DamageRegion>,
}

impl WlrState {
    fn new() -> Self {
        Self {
            shm: None, output: None, screencopy_manager: None,
            linux_dmabuf: None, supported_dma: Vec::new(),
            frame_info: FrameInfo::default(),
        }
    }

    fn reset_frame(&mut self) {
        let fi = &mut self.frame_info;
        fi.got_shm = false; fi.got_dma = false;
        fi.buffer_done = false; fi.ready = false; fi.failed = false;
        fi.damage.clear();
    }

    fn has_linear(&self, format: u32) -> bool {
        self.supported_dma.iter().any(|&(f, m)| f == format && m == 0)
    }
}

// ─── Dispatch impls ────────────────────────────────────────────────────────

impl Dispatch<wl_registry::WlRegistry, ()> for WlrState {
    fn event(state: &mut Self, reg: &wl_registry::WlRegistry, event: wl_registry::Event,
             _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, .. } = event {
            match interface.as_str() {
                "wl_shm" => { state.shm = Some(reg.bind(name, 1, qh, ())); }
                "wl_output" if state.output.is_none() => {
                    state.output = Some(reg.bind(name, 1, qh, ()));
                }
                "zwlr_screencopy_manager_v1" => {
                    state.screencopy_manager = Some(reg.bind(name, 3, qh, ()));
                }
                "zwp_linux_dmabuf_v1" => {
                    state.linux_dmabuf = Some(reg.bind(name, 3, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_shm::WlShm, ()> for WlrState {
    fn event(_: &mut Self, _: &wl_shm::WlShm, _: wl_shm::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<wl_shm_pool::WlShmPool, ()> for WlrState {
    fn event(_: &mut Self, _: &wl_shm_pool::WlShmPool, _: wl_shm_pool::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<wl_buffer::WlBuffer, ()> for WlrState {
    fn event(_: &mut Self, _: &wl_buffer::WlBuffer, _: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<wl_output::WlOutput, ()> for WlrState {
    fn event(_: &mut Self, _: &wl_output::WlOutput, _: wl_output::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<ZwlrScreencopyManagerV1, ()> for WlrState {
    fn event(_: &mut Self, _: &ZwlrScreencopyManagerV1, _: zwlr_screencopy_manager_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwpLinuxDmabufV1, ()> for WlrState {
    fn event(state: &mut Self, _: &ZwpLinuxDmabufV1, event: zwp_linux_dmabuf_v1::Event,
             _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwp_linux_dmabuf_v1::Event::Format { format } => {
                // v1: format-only → LINEAR implicitly supported
                state.supported_dma.push((format, 0));
            }
            zwp_linux_dmabuf_v1::Event::Modifier { format, modifier_hi, modifier_lo } => {
                let modifier = ((modifier_hi as u64) << 32) | modifier_lo as u64;
                state.supported_dma.push((format, modifier));
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpLinuxBufferParamsV1, ()> for WlrState {
    fn event(_: &mut Self, _: &ZwpLinuxBufferParamsV1, _: zwp_linux_buffer_params_v1::Event,
             _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for WlrState {
    fn event(state: &mut Self, _: &ZwlrScreencopyFrameV1, event: zwlr_screencopy_frame_v1::Event,
             _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer { format, width, height, stride } => {
                if let WEnum::Value(fmt) = format {
                    state.frame_info.shm_format = Some(fmt);
                    state.frame_info.shm_width = width;
                    state.frame_info.shm_height = height;
                    state.frame_info.shm_stride = stride;
                    state.frame_info.got_shm = true;
                }
            }
            zwlr_screencopy_frame_v1::Event::LinuxDmabuf { format, width, height } => {
                state.frame_info.dma_format = format;
                state.frame_info.dma_width = width;
                state.frame_info.dma_height = height;
                state.frame_info.got_dma = true;
            }
            zwlr_screencopy_frame_v1::Event::BufferDone => {
                state.frame_info.buffer_done = true;
            }
            zwlr_screencopy_frame_v1::Event::Damage { x, y, width, height } => {
                state.frame_info.damage.push(DamageRegion {
                    x: x as u32, y: y as u32,
                    width: width as u32, height: height as u32,
                });
            }
            zwlr_screencopy_frame_v1::Event::Ready { .. } => {
                state.frame_info.ready = true;
            }
            zwlr_screencopy_frame_v1::Event::Failed => {
                state.frame_info.failed = true;
            }
            _ => {}
        }
    }
}

// ─── SHM buffer ────────────────────────────────────────────────────────────

struct ShmCapBuf {
    shm_buf: ShmBuffer,
    _pool: wl_shm_pool::WlShmPool,
    wl_buf: wl_buffer::WlBuffer,
    width: u32, height: u32,
}

impl ShmCapBuf {
    fn new(shm: &wl_shm::WlShm, w: u32, h: u32, stride: u32,
           fmt: wl_shm::Format, qh: &QueueHandle<WlrState>) -> Self {
        let size = (stride * h) as usize;
        let shm_buf = ShmBuffer::new(size).expect("ShmBuffer::new");
        let pool = shm.create_pool(shm_buf.fd().as_fd(), size as i32, qh, ());
        let wl_buf = pool.create_buffer(0, w as i32, h as i32, stride as i32, fmt, qh, ());
        Self { shm_buf, _pool: pool, wl_buf, width: w, height: h }
    }
    fn needs_resize(&self, w: u32, h: u32) -> bool { self.width != w || self.height != h }
    fn buf(&self) -> &wl_buffer::WlBuffer { &self.wl_buf }
    fn data(&self) -> &[u8] { self.shm_buf.as_slice() }
}

// ─── DMA-BUF buffer ────────────────────────────────────────────────────────

struct DmaBuf {
    bo_ptr: *mut gbm_ffi::GbmBo,
    wl_buf: wl_buffer::WlBuffer,
    ptr: *const u8,
    size: usize,
    pub width: u32,
    pub height: u32,
    pub drm_format: u32,
}

unsafe impl Send for DmaBuf {}

impl Drop for DmaBuf {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.size);
            gbm_ffi::gbm_bo_destroy(self.bo_ptr);
        }
        self.wl_buf.destroy();
    }
}

impl DmaBuf {
    fn try_new(
        gbm: &GbmDevice,
        linux_dmabuf: &ZwpLinuxDmabufV1,
        width: u32, height: u32, drm_format: u32,
        qh: &QueueHandle<WlrState>,
    ) -> Option<Self> {
        let bo_ptr = unsafe {
            gbm_ffi::gbm_bo_create(
                gbm.ptr, width, height, drm_format,
                gbm_ffi::GBM_BO_USE_LINEAR | gbm_ffi::GBM_BO_USE_RENDERING,
            )
        };
        if bo_ptr.is_null() { return None; }

        let stride = unsafe { gbm_ffi::gbm_bo_get_stride(bo_ptr) };
        let raw_fd = unsafe { gbm_ffi::gbm_bo_get_fd(bo_ptr) };
        if raw_fd < 0 { unsafe { gbm_ffi::gbm_bo_destroy(bo_ptr) }; return None; }

        // gbm_bo_get_fd() gives a new fd we own
        let dma_fd: OwnedFd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let size = (stride * height) as usize;

        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(), size,
                libc::PROT_READ, libc::MAP_SHARED,
                dma_fd.as_raw_fd(), 0,
            )
        };
        if ptr == libc::MAP_FAILED {
            unsafe { gbm_ffi::gbm_bo_destroy(bo_ptr) };
            return None;
        }

        let params = linux_dmabuf.create_params(qh, ());
        params.add(dma_fd.as_fd(), 0, 0, stride, 0, 0); // modifier = 0 = LINEAR
        let wl_buf = params.create_immed(
            width as i32, height as i32, drm_format,
            zwp_linux_buffer_params_v1::Flags::empty(),
            qh, (),
        );
        // dma_fd drops here — mmap keeps the buffer alive

        Some(Self {
            bo_ptr, wl_buf,
            ptr: ptr as *const u8,
            size, width, height, drm_format,
        })
    }

    fn needs_resize(&self, w: u32, h: u32, fmt: u32) -> bool {
        self.width != w || self.height != h || self.drm_format != fmt
    }
    fn buf(&self) -> &wl_buffer::WlBuffer { &self.wl_buf }
    fn data(&self) -> &[u8] { unsafe { std::slice::from_raw_parts(self.ptr, self.size) } }
}

// ─── Pixel format conversion ───────────────────────────────────────────────

fn to_rgba(data: &[u8], fmt: u32, w: u32, h: u32, dst: &mut Vec<u8>) {
    match fmt {
        DRM_FORMAT_XRGB8888 | DRM_FORMAT_ARGB8888 => {
            convert_bgrx_to_rgba_inplace(data, w, h, dst);
        }
        DRM_FORMAT_ABGR8888 => {
            dst.resize(data.len(), 0);
            dst.copy_from_slice(data);
        }
        DRM_FORMAT_XBGR8888 => {
            dst.resize(data.len(), 0);
            dst.copy_from_slice(data);
            for px in dst.chunks_exact_mut(4) { px[3] = 255; }
        }
        _ => {
            convert_bgrx_to_rgba_inplace(data, w, h, dst);
        }
    }
}

// ─── GBM device discovery ──────────────────────────────────────────────────

fn open_gbm() -> Option<GbmDevice> {
    for n in 128u32..=135 {
        let path = format!("/dev/dri/renderD{n}");
        if let Some(dev) = GbmDevice::try_open(&path) {
            eprintln!("DMA-BUF: render node {path}");
            return Some(dev);
        }
    }
    None
}

// ─── WlrCapture ───────────────────────────────────────────────────────────

pub struct ProbeResult {
    conn: Connection,
}

pub struct WlrCapture {
    probe: ProbeResult,
    tx: mpsc::SyncSender<Frame>,
    stop: Arc<AtomicBool>,
}

impl WlrCapture {
    pub fn probe() -> Result<ProbeResult> {
        let conn = Connection::connect_to_env()
            .map_err(|e| Error::Wayland(e.to_string()))?;
        let mut eq = conn.new_event_queue::<WlrState>();
        let qh = eq.handle();
        conn.display().get_registry(&qh, ());
        let mut state = WlrState::new();
        eq.roundtrip(&mut state).map_err(|e| Error::Wayland(e.to_string()))?;
        state.screencopy_manager.as_ref().ok_or(Error::NoScreencopyManager)?;
        Ok(ProbeResult { conn })
    }

    pub fn new(probe: ProbeResult, tx: mpsc::SyncSender<Frame>, stop: Arc<AtomicBool>) -> Self {
        Self { probe, tx, stop }
    }

}

fn send_frame(tx: &mpsc::SyncSender<Frame>, frame: Frame) -> Result<()> {
    match tx.try_send(frame) {
        Ok(()) | Err(mpsc::TrySendError::Full(_)) => Ok(()),
        Err(mpsc::TrySendError::Disconnected(_)) => Err(Error::ConsumerDisconnected),
    }
}

// Wait for a capture_output() request's buffer hints (Buffer/LinuxDmabuf/BufferDone,
// or Failed). Shared between the initial capture and the DMA-rejection recapture below.
fn wait_for_buffer_hint(
    eq: &mut wayland_client::EventQueue<WlrState>,
    state: &mut WlrState,
) -> Result<()> {
    loop {
        eq.blocking_dispatch(state).map_err(|e| Error::Wayland(e.to_string()))?;
        let fi = &state.frame_info;
        if fi.failed || fi.buffer_done { break; }
        // v1/v2 fallback: no buffer_done, just Buffer event
        if fi.got_shm && state.linux_dmabuf.is_none() { break; }
    }
    Ok(())
}

impl CaptureBackend for WlrCapture {
    fn run(self: Box<Self>, frame_duration: Duration) -> Result<()> {
        let WlrCapture { probe, tx, stop } = *self;
        let conn = probe.conn;
        let mut eq = conn.new_event_queue::<WlrState>();
        let qh = eq.handle();
        conn.display().get_registry(&qh, ());
        let mut state = WlrState::new();
        eq.roundtrip(&mut state).map_err(|e| Error::Wayland(e.to_string()))?;
        // Second roundtrip to collect DMA-BUF format/modifier events
        if state.linux_dmabuf.is_some() {
            eq.roundtrip(&mut state).map_err(|e| Error::Wayland(e.to_string()))?;
        }

        let shm = state.shm.clone().ok_or_else(|| Error::Wayland("wl_shm не знайдено".into()))?;
        let output = state.output.take().ok_or(Error::NoOutput)?;
        let manager = state.screencopy_manager.take().ok_or(Error::NoScreencopyManager)?;

        let gbm = if state.linux_dmabuf.is_some() { open_gbm() } else { None };
        if gbm.is_none() && state.linux_dmabuf.is_some() {
            eprintln!("DMA-BUF: GBM недоступний → SHM");
        }

        let mut shm_buf: Option<ShmCapBuf> = None;
        let mut dma_buf: Option<DmaBuf> = None;
        let mut rgba_buf: Vec<u8> = Vec::new();

        loop {
            let tick = std::time::Instant::now();
            if stop.load(Ordering::Relaxed) { break; }

            state.reset_frame();
            let mut frame = manager.capture_output(1, &output, &qh, ());
            eq.flush().map_err(|e| Error::Wayland(e.to_string()))?;
            wait_for_buffer_hint(&mut eq, &mut state)?;

            if state.frame_info.failed { frame.destroy(); return Err(Error::FrameFailed); }

            let mut damage = state.frame_info.damage.clone();
            let fi = &state.frame_info;

            let try_dma = gbm.is_some()
                && state.linux_dmabuf.is_some()
                && fi.got_dma
                && state.has_linear(fi.dma_format);

            if try_dma {
                let (dw, dh, dfmt) = (fi.dma_width, fi.dma_height, fi.dma_format);
                let ld = state.linux_dmabuf.as_ref().unwrap();
                let gd = gbm.as_ref().unwrap();

                if dma_buf.as_ref().map_or(true, |b| b.needs_resize(dw, dh, dfmt)) {
                    dma_buf = DmaBuf::try_new(gd, ld, dw, dh, dfmt, &qh);
                    if dma_buf.is_none() {
                        eprintln!("DMA-BUF: не вдалось створити буфер → SHM");
                    }
                }

                if let Some(ref db) = dma_buf {
                    frame.copy(db.buf());
                    eq.flush().map_err(|e| Error::Wayland(e.to_string()))?;
                    while !state.frame_info.ready && !state.frame_info.failed {
                        eq.blocking_dispatch(&mut state).map_err(|e| Error::Wayland(e.to_string()))?;
                    }

                    if state.frame_info.ready {
                        let db = dma_buf.as_ref().unwrap();
                        to_rgba(db.data(), db.drm_format, db.width, db.height, &mut rgba_buf);
                        frame.destroy();
                        send_frame(&tx,Frame::new(std::mem::take(&mut rgba_buf), db.width, db.height, damage))?;
                        let e = tick.elapsed();
                        if e < frame_duration { std::thread::sleep(frame_duration - e); }
                        continue;
                    }

                    // Per wlr-screencopy protocol, `failed` terminates this
                    // frame object ("After receiving this event, the client
                    // should destroy the object") — it cannot be reused for
                    // a second copy() call, so a genuine SHM fallback needs
                    // a fresh capture_output() rather than reusing `frame`.
                    eprintln!("DMA-BUF: compositor відхилив → SHM fallback (перезахоплення)");
                    frame.destroy();
                    dma_buf = None;

                    state.reset_frame();
                    frame = manager.capture_output(1, &output, &qh, ());
                    eq.flush().map_err(|e| Error::Wayland(e.to_string()))?;
                    wait_for_buffer_hint(&mut eq, &mut state)?;

                    if state.frame_info.failed { frame.destroy(); return Err(Error::FrameFailed); }
                    damage = state.frame_info.damage.clone();
                }
            }

            // ── SHM path ──────────────────────────────────────────────────
            let fi = &state.frame_info;
            if !fi.got_shm { frame.destroy(); continue; }

            let (sw, sh, ss) = (fi.shm_width, fi.shm_height, fi.shm_stride);
            let sfmt = fi.shm_format.unwrap_or(wl_shm::Format::Xrgb8888);
            if shm_buf.as_ref().map_or(true, |b| b.needs_resize(sw, sh)) {
                shm_buf = Some(ShmCapBuf::new(&shm, sw, sh, ss, sfmt, &qh));
            }
            let sb = shm_buf.as_ref().unwrap();
            frame.copy(sb.buf());
            eq.flush().map_err(|e| Error::Wayland(e.to_string()))?;
            while !state.frame_info.ready && !state.frame_info.failed {
                eq.blocking_dispatch(&mut state).map_err(|e| Error::Wayland(e.to_string()))?;
            }
            if state.frame_info.failed { frame.destroy(); return Err(Error::FrameFailed); }

            let sb = shm_buf.as_ref().unwrap();
            convert_bgrx_to_rgba_inplace(sb.data(), sb.width, sb.height, &mut rgba_buf);
            frame.destroy();
            send_frame(&tx,Frame::new(std::mem::take(&mut rgba_buf), sb.width, sb.height, damage))?;

            let e = tick.elapsed();
            if e < frame_duration { std::thread::sleep(frame_duration - e); }
        }

        Ok(())
    }
}
