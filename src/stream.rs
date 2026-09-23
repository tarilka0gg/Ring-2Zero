//! One streaming session: glues a capture feed to the `screen` DataChannel.
//!
//! Two halves:
//! - a processing thread owning the [`Pipeline`] (CPU-heavy: diff, merge,
//!   WebP encode), fed by the capture thread's frame channel;
//! - an async send loop owning the [`AckTracker`] and the DataChannel.
//!
//! Lost batches flow back from the send loop to the processing thread as
//! epoch-tagged grid cells; the pipeline itself drops ones from an older
//! tile grid, so a resize mid-flight can't misapply them.

use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::pipeline::{EncodedFrame, Pipeline};
use crate::protocol;
use crate::transport::{self, AckTracker, LostCells, ACK_TIMEOUT};

/// How often the send loop checks for expired ACKs while no new frames
/// arrive — without this, a batch lost right before the screen went static
/// would never be re-sent.
const ACK_POLL: Duration = Duration::from_millis(50);

/// Streams `frame_rx` to `dc` until the channel closes or a send fails.
pub async fn run_session(config: Config, dc: Arc<RTCDataChannel>, frame_rx: mpsc::Receiver<Frame>) -> Result<()> {
    log::info!("Client connected, streaming");

    let (ack_tx, mut ack_rx) = tokio::sync::mpsc::unbounded_channel::<u32>();
    dc.on_message(Box::new(move |msg: DataChannelMessage| {
        if !msg.is_string {
            if let Some(seq) = protocol::decode_ack(&msg.data) {
                let _ = ack_tx.send(seq);
            }
        }
        Box::pin(async {})
    }));

    let (encoded_tx, mut encoded_rx) = tokio::sync::mpsc::channel::<EncodedFrame>(4);
    let (lost_tx, lost_rx) = mpsc::channel::<LostCells>();
    let process_handle = std::thread::Builder::new()
        .name("r2z-pipeline".into())
        .spawn(move || processing_loop(config, frame_rx, lost_rx, encoded_tx))?;

    let mut acks = AckTracker::new(ACK_TIMEOUT);
    let mut ticker = tokio::time::interval(ACK_POLL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut stats = SessionStats::default();

    let result: Result<()> = async {
        loop {
            let frame = tokio::select! {
                frame = encoded_rx.recv() => match frame {
                    Some(f) => Some(f),
                    None => return Ok(()),
                },
                _ = ticker.tick() => None,
            };

            while let Ok(seq) = ack_rx.try_recv() {
                acks.ack(seq);
            }
            for lost in acks.take_expired(Instant::now()) {
                log::debug!("Batch lost in transit, re-queuing {} cells", lost.1.len());
                let _ = lost_tx.send(lost);
            }

            let Some(mut frame) = frame else { continue };
            if let Some((w, h)) = frame.header {
                transport::send_header(&dc, w, h).await?;
            }
            if frame.tiles.is_empty() {
                continue;
            }
            let queue_ms = frame.produced_at.elapsed().as_secs_f64() * 1000.0;
            let seq = acks.register(frame.epoch, std::mem::take(&mut frame.ack_indices), Instant::now());
            let send_start = Instant::now();
            let bytes = transport::send_tiles(&dc, seq, &frame).await?;
            stats.record(&frame, bytes, queue_ms, send_start.elapsed().as_secs_f64() * 1000.0);
        }
    }
    .await;

    // Closing our end of the channel makes the processing thread exit at its
    // next iteration; join it off the async runtime, bounded, so a wedged
    // encoder can't stall this worker.
    drop(encoded_rx);
    match tokio::time::timeout(Duration::from_secs(5), tokio::task::spawn_blocking(move || process_handle.join())).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(e))) => return Err(Error::WebRTC(format!("Processing thread panicked: {e:?}"))),
        Ok(Err(e)) => log::error!("Failed to join processing thread: {e}"),
        Err(_) => log::warn!("Processing thread did not finish within 5s"),
    }
    log::info!("Session ended after {} frames", stats.frames);
    result
}

fn processing_loop(
    config: Config,
    frame_rx: mpsc::Receiver<Frame>,
    lost_rx: mpsc::Receiver<LostCells>,
    encoded_tx: tokio::sync::mpsc::Sender<EncodedFrame>,
) {
    let frame_duration = config.frame_duration();
    let mut pipeline = Pipeline::new(config);

    while !encoded_tx.is_closed() {
        let deadline = Instant::now() + frame_duration;

        while let Ok((epoch, cells)) = lost_rx.try_recv() {
            if !pipeline.invalidate(epoch, &cells) {
                log::debug!("Dropped {} lost cells from a stale tile grid", cells.len());
            }
        }

        let frame = match frame_rx.recv_timeout(frame_duration) {
            Ok(f) => f,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                log::warn!("Capture thread disconnected");
                break;
            }
        };

        if let Some(out) = pipeline.process(&frame) {
            if encoded_tx.blocking_send(out).is_err() {
                break;
            }
        }

        if let Some(rest) = deadline.checked_duration_since(Instant::now()) {
            std::thread::sleep(rest);
        }
    }
}

#[derive(Default)]
struct SessionStats {
    frames: u64,
    avg_process_ms: f64,
}

impl SessionStats {
    fn record(&mut self, frame: &EncodedFrame, bytes: usize, queue_ms: f64, send_ms: f64) {
        self.frames += 1;
        self.avg_process_ms += (frame.process_ms - self.avg_process_ms) / self.frames as f64;
        log::debug!(
            "{} tiles / {:.1} kbit / {:.1} ms (avg {:.1}) / queue {:.1} ms / send {:.1} ms",
            frame.tiles.len(),
            bytes as f64 * 8.0 / 1000.0,
            frame.process_ms,
            self.avg_process_ms,
            queue_ms,
            send_ms
        );
    }
}
