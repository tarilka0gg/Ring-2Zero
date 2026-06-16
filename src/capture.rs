use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::shm::ShmBuffer;
use crate::convert::convert_bgrx_to_rgba_inplace;

use std::os::unix::io::AsFd;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::Duration;

use wayland_client::{
    protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool},
    Connection, Dispatch, QueueHandle, WEnum,
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
};

struct WaylandState {
    shm: Option<wl_shm::WlShm>,
    output: Option<wl_output::WlOutput>,
    screencopy_manager: Option<ZwlrScreencopyManagerV1>,
    frame_info: FrameInfo,
}

#[derive(Default)]
struct FrameInfo {
    width: u32,
    height: u32,
    stride: u32,
    format: Option<wl_shm::Format>,
    got_buffer_event: bool,
    ready: bool,
    failed: bool,
    damage_regions: Vec<DamageRegion>,
}

#[derive(Clone, Copy, Debug)]
pub struct DamageRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl WaylandState {
    fn new() -> Self {
        Self {
            shm: None,
            output: None,
            screencopy_manager: None,
            frame_info: FrameInfo::default(),
        }
    }

    fn reset_frame_state(&mut self) {
        self.frame_info.got_buffer_event = false;
        self.frame_info.ready = false;
        self.frame_info.failed = false;
        self.frame_info.damage_regions.clear();
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for WaylandState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, .. } = event {
            match interface.as_str() {
                "wl_shm" => {
                    state.shm = Some(registry.bind::<wl_shm::WlShm, _, _>(name, 1, qh, ()));
                }
                "wl_output" if state.output.is_none() => {
                    state.output = Some(registry.bind::<wl_output::WlOutput, _, _>(name, 1, qh, ()));
                }
                "zwlr_screencopy_manager_v1" => {
                    state.screencopy_manager =
                        Some(registry.bind::<ZwlrScreencopyManagerV1, _, _>(name, 3, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_shm::WlShm, ()> for WaylandState {
    fn event(_: &mut Self, _: &wl_shm::WlShm, _: wl_shm::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for WaylandState {
    fn event(_: &mut Self, _: &wl_shm_pool::WlShmPool, _: wl_shm_pool::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_buffer::WlBuffer, ()> for WaylandState {
    fn event(_: &mut Self, _: &wl_buffer::WlBuffer, _: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_output::WlOutput, ()> for WaylandState {
    fn event(_: &mut Self, _: &wl_output::WlOutput, _: wl_output::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for WaylandState {
    fn event(_: &mut Self, _: &ZwlrScreencopyManagerV1, _: zwlr_screencopy_manager_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer { format, width, height, stride } => {
                if let WEnum::Value(fmt) = format {
                    state.frame_info.format = Some(fmt);
                    state.frame_info.width = width;
                    state.frame_info.height = height;
                    state.frame_info.stride = stride;
                    state.frame_info.got_buffer_event = true;
                }
            }
            zwlr_screencopy_frame_v1::Event::Damage { x, y, width, height } => {
                state.frame_info.damage_regions.push(DamageRegion {
                    x: x as u32,
                    y: y as u32,
                    width: width as u32,
                    height: height as u32,
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

struct CaptureBuffer {
    shm_buffer: ShmBuffer,
    _pool: wl_shm_pool::WlShmPool,
    buffer: wl_buffer::WlBuffer,
    width: u32,
    height: u32,
}

impl CaptureBuffer {
    fn new(
        shm: &wl_shm::WlShm,
        width: u32,
        height: u32,
        stride: u32,
        format: wl_shm::Format,
        qh: &QueueHandle<WaylandState>,
    ) -> Self {
        let size = (stride * height) as usize;
        let shm_buffer = ShmBuffer::new(size).expect("Failed to create shared memory buffer");
        let pool = shm.create_pool(shm_buffer.fd().as_fd(), size as i32, qh, ());
        let buffer = pool.create_buffer(0, width as i32, height as i32, stride as i32, format, qh, ());

        Self {
            shm_buffer,
            _pool: pool,
            buffer,
            width,
            height,
        }
    }

    fn needs_resize(&self, width: u32, height: u32) -> bool {
        self.width != width || self.height != height
    }

    fn buffer(&self) -> &wl_buffer::WlBuffer {
        &self.buffer
    }

    fn data(&self) -> &[u8] {
        self.shm_buffer.as_slice()
    }
}

pub struct ScreenCapture {
    frame_tx: mpsc::SyncSender<Frame>,
    stop: Arc<AtomicBool>,
    rgba_buffer: Vec<u8>, // Reusable buffer
}

impl ScreenCapture {
    pub fn new(frame_tx: mpsc::SyncSender<Frame>, stop: Arc<AtomicBool>) -> Self {
        Self {
            frame_tx,
            stop,
            rgba_buffer: Vec::new(),
        }
    }

    pub fn run(mut self, frame_duration: Duration) -> Result<()> {
        let conn = Connection::connect_to_env()
            .map_err(|e| Error::Wayland(e.to_string()))?;

        let mut eq = conn.new_event_queue::<WaylandState>();
        let qh = eq.handle();
        conn.display().get_registry(&qh, ());

        let mut state = WaylandState::new();
        eq.roundtrip(&mut state)
            .map_err(|e| Error::Wayland(e.to_string()))?;

        let shm = state.shm.clone()
            .ok_or_else(|| Error::Wayland("wl_shm не знайдено".into()))?;
        let output = state.output.take().ok_or(Error::NoOutput)?;
        let manager = state.screencopy_manager.take().ok_or(Error::NoScreencopyManager)?;

        let mut capture_buffer: Option<CaptureBuffer> = None;

        loop {
            let tick = std::time::Instant::now();
            if self.stop.load(Ordering::Relaxed) {
                break;
            }

            state.reset_frame_state();

            let frame = manager.capture_output(1, &output, &qh, ());
            eq.flush().map_err(|e| Error::Wayland(e.to_string()))?;

            while !state.frame_info.got_buffer_event && !state.frame_info.failed {
                eq.blocking_dispatch(&mut state)
                    .map_err(|e| Error::Wayland(e.to_string()))?;
            }

            if state.frame_info.failed {
                frame.destroy();
                return Err(Error::FrameFailed);
            }

            let width = state.frame_info.width;
            let height = state.frame_info.height;
            let stride = state.frame_info.stride;
            let format = state.frame_info.format.unwrap_or(wl_shm::Format::Xrgb8888);

            if capture_buffer.as_ref().map_or(true, |b| b.needs_resize(width, height)) {
                capture_buffer = Some(CaptureBuffer::new(&shm, width, height, stride, format, &qh));
            }

            let buffer = capture_buffer.as_ref().unwrap();
            frame.copy(buffer.buffer());
            eq.flush().map_err(|e| Error::Wayland(e.to_string()))?;

            while !state.frame_info.ready && !state.frame_info.failed {
                eq.blocking_dispatch(&mut state)
                    .map_err(|e| Error::Wayland(e.to_string()))?;
            }

            if state.frame_info.failed {
                return Err(Error::FrameFailed);
            }

            convert_bgrx_to_rgba_inplace(buffer.data(), width, height, &mut self.rgba_buffer);
            let damage_regions = state.frame_info.damage_regions.clone();
            let frame_data = std::mem::take(&mut self.rgba_buffer);

            match self.frame_tx.try_send(Frame::new(frame_data, width, height, damage_regions)) {
                Ok(()) => {
                    // Frame sent successfully
                }
                Err(mpsc::TrySendError::Full(_frame)) => {
                    // Receiver slow - frame drop is acceptable for real-time streaming
                    // Better to skip frame than block capture thread
                }
                Err(mpsc::TrySendError::Disconnected(_frame)) => {
                    // Receiver dropped - exit gracefully
                    println!("Frame receiver disconnected, stopping capture");
                    return Ok(());
                }
            }

            let elapsed = tick.elapsed();
            if elapsed < frame_duration {
                std::thread::sleep(frame_duration - elapsed);
            }
        }

        Ok(())
    }
}
