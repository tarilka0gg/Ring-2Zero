#[cfg(feature = "pipewire_capture")]
pub mod pipewire;
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

/// The operator's preferred output name (e.g. `DP-1`, `eDP-1`) from
/// `RING2ZERO_OUTPUT`, if set to a non-empty value. Shared by every capture
/// backend that enumerates named outputs.
pub(super) fn desired_output_name() -> Option<String> {
    std::env::var("RING2ZERO_OUTPUT")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Picks the candidate whose name matches `want`, or the first candidate if
/// `want` is `None` or matches none of them (logging which, and — on a
/// requested-but-missing name — what was actually available, so a typo'd
/// `RING2ZERO_OUTPUT` is diagnosable from the startup log instead of just
/// silently capturing the wrong screen).
pub(super) fn select_output<'a, T>(
    candidates: &'a [(T, Option<String>)],
    want: Option<&str>,
) -> Option<&'a T> {
    if let Some(want) = want {
        if let Some((output, _)) = candidates
            .iter()
            .find(|(_, name)| name.as_deref() == Some(want))
        {
            log::info!("Capturing output {want:?} (RING2ZERO_OUTPUT)");
            return Some(output);
        }
        let available: Vec<&str> = candidates
            .iter()
            .filter_map(|(_, n)| n.as_deref())
            .collect();
        log::warn!(
            "RING2ZERO_OUTPUT={want:?} does not match any connected output {available:?};              using the first one instead"
        );
    }
    candidates.first().map(|(output, _)| output)
}

pub(super) trait CaptureBackend: Send {
    fn run(self: Box<Self>, frame_duration: Duration) -> Result<()>;
}

pub struct ScreenCapture {
    backend: Box<dyn CaptureBackend>,
}

impl ScreenCapture {
    pub fn new(frame_tx: mpsc::SyncSender<Frame>, stop: Arc<AtomicBool>) -> Result<Self> {
        let backend = Self::detect(frame_tx, stop)?;
        Ok(Self { backend })
    }

    fn detect(
        tx: mpsc::SyncSender<Frame>,
        stop: Arc<AtomicBool>,
    ) -> Result<Box<dyn CaptureBackend>> {
        // 1. wlr-screencopy (niri, sway, wlroots DEs)
        if std::env::var("WAYLAND_DISPLAY").is_ok() || std::env::var("WAYLAND_SOCKET").is_ok() {
            match wlr::WlrCapture::probe() {
                Ok(probe) => {
                    log::info!("Capture: wlr-screencopy (DMA-BUF preferred)");
                    return Ok(Box::new(wlr::WlrCapture::new(probe, tx, stop)));
                }
                Err(e) => log::warn!("wlr-screencopy: {e}"),
            }
        }

        // 2. PipeWire via xdg-desktop-portal (GNOME, KDE, X11)
        #[cfg(feature = "pipewire_capture")]
        {
            log::info!("Capture: PipeWire (portal)");
            Ok(Box::new(pipewire::PipeWireCapture::new(tx, stop)))
        }

        #[cfg(not(feature = "pipewire_capture"))]
        {
            log::error!(
                "No capture backend available.\n\
                 Wayland + wlr-screencopy are required, or compile with --features pipewire_capture"
            );
            Err(crate::error::Error::NoBackend)
        }
    }

    pub fn run(self, frame_duration: Duration) -> Result<()> {
        self.backend.run(frame_duration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates(names: &[Option<&str>]) -> Vec<(u32, Option<String>)> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| (i as u32, n.map(str::to_owned)))
            .collect()
    }

    #[test]
    fn no_preference_picks_the_first_output() {
        let c = candidates(&[Some("DP-1"), Some("eDP-1")]);
        assert_eq!(select_output(&c, None), Some(&0));
    }

    #[test]
    fn matching_preference_picks_that_output_regardless_of_order() {
        let c = candidates(&[Some("DP-1"), Some("eDP-1"), Some("HDMI-A-1")]);
        assert_eq!(select_output(&c, Some("HDMI-A-1")), Some(&2));
    }

    #[test]
    fn unmatched_preference_falls_back_to_the_first_output() {
        let c = candidates(&[Some("DP-1"), Some("eDP-1")]);
        assert_eq!(select_output(&c, Some("DP-2")), Some(&0));
    }

    #[test]
    fn empty_candidate_list_selects_nothing() {
        let c: Vec<(u32, Option<String>)> = Vec::new();
        assert_eq!(select_output(&c, None), None);
        assert_eq!(select_output(&c, Some("DP-1")), None);
    }

    #[test]
    fn an_unnamed_output_can_still_be_the_fallback() {
        // A compositor whose wl_output predates version 4 (no Name event)
        // reports no name at all; without a preference it should still be
        // usable as the (only) candidate.
        let c = candidates(&[None]);
        assert_eq!(select_output(&c, None), Some(&0));
    }
}
