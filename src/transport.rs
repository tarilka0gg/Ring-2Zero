//! Server side of the `screen` DataChannel: framing a processed frame onto
//! the wire and tracking which batches the client has acknowledged.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use webrtc::data_channel::RTCDataChannel;

use crate::error::{Error, Result};
use crate::pipeline::EncodedFrame;
use crate::protocol::{self, TileRect};

/// A batch unacknowledged for this long is treated as lost.
pub const ACK_TIMEOUT: Duration = Duration::from_millis(150);

/// Grid cells of a lost batch, tagged with the tile-grid epoch they belong to.
pub type LostCells = (u64, Vec<usize>);

struct InFlight {
    sent_at: Instant,
    epoch: u64,
    cells: Vec<usize>,
}

/// Tracks sent-but-unacknowledged tile batches. Pure state, no I/O.
///
/// The client ACKs a batch only after decoding every tile in it, so a batch
/// that times out had at least one tile lost or undecodable. Every grid cell
/// the batch covered — all cells of a merged tile, not only its
/// representative one — is handed back for re-sending; otherwise up to 15/16
/// of a lost 4×4 merged region would keep stale content forever.
pub struct AckTracker {
    timeout: Duration,
    next_seq: u32,
    in_flight: HashMap<u32, InFlight>,
}

impl AckTracker {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            next_seq: 0,
            in_flight: HashMap::new(),
        }
    }

    /// Records a batch about to be sent and returns its sequence number.
    pub fn register(&mut self, epoch: u64, cells: Vec<usize>, now: Instant) -> u32 {
        self.next_seq = self.next_seq.wrapping_add(1);
        self.in_flight.insert(
            self.next_seq,
            InFlight {
                sent_at: now,
                epoch,
                cells,
            },
        );
        self.next_seq
    }

    /// Marks a batch delivered. Unknown or already-expired sequence numbers
    /// (a late ACK) are ignored.
    pub fn ack(&mut self, seq: u32) {
        self.in_flight.remove(&seq);
    }

    /// Removes and returns every batch older than the timeout.
    pub fn take_expired(&mut self, now: Instant) -> Vec<LostCells> {
        let expired: Vec<u32> = self
            .in_flight
            .iter()
            .filter(|(_, f)| now.duration_since(f.sent_at) > self.timeout)
            .map(|(&seq, _)| seq)
            .collect();
        expired
            .into_iter()
            .filter_map(|seq| self.in_flight.remove(&seq))
            .map(|f| (f.epoch, f.cells))
            .collect()
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }
}

pub async fn send_header(dc: &Arc<RTCDataChannel>, width: u32, height: u32) -> Result<()> {
    let header = protocol::encode_header(width, height).ok_or_else(|| {
        Error::WebRTC(format!(
            "Screen resolution {width}×{height} exceeds protocol limit"
        ))
    })?;
    dc.send(&Bytes::copy_from_slice(&header)).await?;
    Ok(())
}

/// Sends a frame's sequence packet and tile packets. Returns bytes of WebP sent.
pub async fn send_tiles(dc: &Arc<RTCDataChannel>, seq: u32, frame: &EncodedFrame) -> Result<usize> {
    dc.send(&Bytes::copy_from_slice(&protocol::encode_seq(
        seq,
        frame.tiles.len() as u32,
    )))
    .await?;
    let tiles = frame.tiles.iter().zip(&frame.encoded).map(|(t, webp)| {
        (
            TileRect {
                x: t.x,
                y: t.y,
                width: t.width,
                height: t.height,
            },
            webp.as_slice(),
        )
    });
    for packet in protocol::pack_tiles(tiles) {
        dc.send(&packet).await?;
    }
    Ok(frame.encoded.iter().map(Vec::len).sum())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn sequence_numbers_increase_from_one() {
        let mut t = AckTracker::new(ACK_TIMEOUT);
        let now = Instant::now();
        assert_eq!(t.register(0, vec![1], now), 1);
        assert_eq!(t.register(0, vec![2], now), 2);
    }

    #[test]
    fn acked_batches_never_expire() {
        let mut t = AckTracker::new(ACK_TIMEOUT);
        let now = Instant::now();
        let seq = t.register(0, vec![1, 2], now);
        t.ack(seq);
        assert!(t.take_expired(now + ACK_TIMEOUT * 10).is_empty());
        assert_eq!(t.in_flight(), 0);
    }

    #[test]
    fn unacked_batch_expires_only_after_the_timeout_with_its_epoch_and_cells() {
        let mut t = AckTracker::new(ACK_TIMEOUT);
        let now = Instant::now();
        t.register(3, vec![4, 5, 6], now);
        assert!(t.take_expired(now + ACK_TIMEOUT).is_empty());
        assert_eq!(
            t.take_expired(now + ACK_TIMEOUT + MS),
            vec![(3, vec![4, 5, 6])]
        );
        assert!(
            t.take_expired(now + ACK_TIMEOUT * 10).is_empty(),
            "reported once"
        );
    }

    #[test]
    fn late_ack_for_an_expired_batch_is_harmless() {
        let mut t = AckTracker::new(ACK_TIMEOUT);
        let now = Instant::now();
        let seq = t.register(0, vec![1], now);
        t.take_expired(now + ACK_TIMEOUT * 2);
        t.ack(seq);
        t.ack(999);
        assert_eq!(t.in_flight(), 0);
    }

    #[test]
    fn sequence_wraps_around() {
        let mut t = AckTracker::new(ACK_TIMEOUT);
        t.next_seq = u32::MAX;
        assert_eq!(t.register(0, vec![], Instant::now()), 0);
    }
}
