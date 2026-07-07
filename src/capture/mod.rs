pub mod wlr;
#[cfg(feature = "pipewire_capture")]
pub mod pipewire;

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
        // 1. wlr-screencopy (niri, sway, wlroots DEs)
        if std::env::var("WAYLAND_DISPLAY").is_ok() || std::env::var("WAYLAND_SOCKET").is_ok() {
            match wlr::WlrCapture::probe() {
                Ok(probe) => {
                    eprintln!("Capture: wlr-screencopy (DMA-BUF preferred)");
                    return Box::new(wlr::WlrCapture::new(probe, tx, stop));
                }
                Err(e) => eprintln!("wlr-screencopy: {e}"),
            }
        }

        // 2. PipeWire via xdg-desktop-portal (GNOME, KDE, X11)
        #[cfg(feature = "pipewire_capture")]
        {
            eprintln!("Capture: PipeWire (portal)");
            return Box::new(pipewire::PipeWireCapture::new(tx, stop));
        }

        #[cfg(not(feature = "pipewire_capture"))]
        panic!(
            "Немає доступного бекенду захоплення.\n\
             Wayland + wlr-screencopy потрібні, або скомпілюйте з --features pipewire_capture"
        );
    }

    pub fn run(self, frame_duration: Duration) -> Result<()> {
        self.backend.run(frame_duration)
    }
}
