pub mod wlr;

use crate::error::Result;
use crate::frame::Frame;

use std::sync::{atomic::AtomicBool, mpsc, Arc};
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct DamageRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

pub(super) trait CaptureBackend: Send {
    fn run(self: Box<Self>, frame_duration: Duration) -> Result<()>;
}

pub struct ScreenCapture {
    backend: Box<dyn CaptureBackend>,
}

impl ScreenCapture {
    pub fn new(frame_tx: mpsc::SyncSender<Frame>, stop: Arc<AtomicBool>) -> Self {
        let backend = Self::detect(frame_tx, stop);
        Self { backend }
    }

    fn detect(tx: mpsc::SyncSender<Frame>, stop: Arc<AtomicBool>) -> Box<dyn CaptureBackend> {
        if std::env::var("WAYLAND_DISPLAY").is_ok() || std::env::var("WAYLAND_SOCKET").is_ok() {
            match wlr::WlrCapture::probe() {
                Ok(probe) => {
                    eprintln!("Capture: wlr-screencopy");
                    return Box::new(wlr::WlrCapture::new(probe, tx, stop));
                }
                Err(e) => eprintln!("wlr-screencopy: {e}"),
            }
        }
        panic!("Немає доступного бекенду захоплення (Wayland + wlr-screencopy потрібні)");
    }

    pub fn run(self, frame_duration: Duration) -> Result<()> {
        self.backend.run(frame_duration)
    }
}
