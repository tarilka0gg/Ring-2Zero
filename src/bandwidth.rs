//! Runtime quality adaptation based on how full the DataChannel's own send
//! buffer is. Pure state, no I/O — `stream.rs` samples
//! `RTCDataChannel::buffered_amount()` on a timer and feeds it in; the
//! pipeline reads the resulting scale once per frame.

/// Quality never drops below this fraction of the configured value, so a
/// congested link still gets *something* recognisable rather than a
/// near-blank tile.
const MIN_SCALE: f32 = 0.3;

/// Above this many buffered bytes, the link can't keep up with what's
/// already been handed to it — back off.
const HIGH_WATERMARK: usize = 256 * 1024;

/// Below this many buffered bytes, the outgoing queue is essentially
/// draining as fast as we fill it — safe to (slowly) recover.
const LOW_WATERMARK: usize = 32 * 1024;

/// How much to cut on a single congested sample.
const BACKOFF_STEP: f32 = 0.15;

/// How much to add back on a single sustained-clear recovery step.
const RECOVER_STEP: f32 = 0.05;

/// Consecutive clear samples required before recovering one step — recovery
/// is deliberately slower than backoff (multiple ticks of a small step
/// versus one tick of a big one), so a flaky link that oscillates around
/// the watermark trends down, not into a backoff/recover thrash loop.
const RECOVER_AFTER: u32 = 3;

/// Tracks a DataChannel's congestion state and derives a `[MIN_SCALE, 1.0]`
/// multiplier for tile quality from it.
pub struct BandwidthController {
    scale: f32,
    clear_streak: u32,
}

impl Default for BandwidthController {
    fn default() -> Self {
        Self {
            scale: 1.0,
            clear_streak: 0,
        }
    }
}

impl BandwidthController {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds in the DataChannel's current `buffered_amount()` and returns
    /// the updated quality scale.
    pub fn sample(&mut self, buffered_amount: usize) -> f32 {
        if buffered_amount > HIGH_WATERMARK {
            self.clear_streak = 0;
            self.scale = (self.scale - BACKOFF_STEP).max(MIN_SCALE);
        } else if buffered_amount < LOW_WATERMARK {
            self.clear_streak += 1;
            if self.clear_streak >= RECOVER_AFTER {
                self.clear_streak = 0;
                self.scale = (self.scale + RECOVER_STEP).min(1.0);
            }
        } else {
            // Between the watermarks: holding steady is itself useful
            // information (neither congested nor clearly draining), so
            // reset the recovery streak without changing scale.
            self.clear_streak = 0;
        }
        self.scale
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_full_quality() {
        assert_eq!(BandwidthController::new().scale(), 1.0);
    }

    #[test]
    fn congestion_backs_off_immediately() {
        let mut c = BandwidthController::new();
        assert_eq!(c.sample(HIGH_WATERMARK + 1), 1.0 - BACKOFF_STEP);
    }

    #[test]
    fn repeated_congestion_stops_at_the_floor() {
        let mut c = BandwidthController::new();
        for _ in 0..50 {
            c.sample(HIGH_WATERMARK + 1);
        }
        assert_eq!(c.scale(), MIN_SCALE);
    }

    #[test]
    fn a_single_clear_sample_does_not_recover_alone() {
        let mut c = BandwidthController::new();
        c.sample(HIGH_WATERMARK + 1); // knock it down first
        let after_backoff = c.scale();
        c.sample(LOW_WATERMARK - 1);
        assert_eq!(
            c.scale(),
            after_backoff,
            "one clear sample shouldn't move it yet"
        );
    }

    #[test]
    fn recovery_needs_a_sustained_clear_streak() {
        let mut c = BandwidthController::new();
        c.sample(HIGH_WATERMARK + 1);
        let after_backoff = c.scale();
        for _ in 0..RECOVER_AFTER - 1 {
            c.sample(LOW_WATERMARK - 1);
        }
        assert_eq!(c.scale(), after_backoff, "not enough clear samples yet");
        c.sample(LOW_WATERMARK - 1);
        assert!(
            c.scale() > after_backoff,
            "streak completed, should recover one step"
        );
    }

    #[test]
    fn recovery_never_exceeds_full_quality() {
        let mut c = BandwidthController::new();
        for _ in 0..100 {
            c.sample(0);
        }
        assert_eq!(c.scale(), 1.0);
    }

    #[test]
    fn mid_range_samples_hold_steady_and_reset_the_recovery_streak() {
        let mut c = BandwidthController::new();
        c.sample(HIGH_WATERMARK + 1);
        let after_backoff = c.scale();
        c.sample(LOW_WATERMARK - 1); // 1 of RECOVER_AFTER clear samples
        c.sample((LOW_WATERMARK + HIGH_WATERMARK) / 2); // resets the streak
        for _ in 0..RECOVER_AFTER - 1 {
            c.sample(LOW_WATERMARK - 1);
        }
        assert_eq!(
            c.scale(),
            after_backoff,
            "streak was reset by the mid-range sample"
        );
    }

    #[test]
    fn oscillating_congestion_trends_down_not_sideways() {
        // Backoff is one big step; recovery needs several small ones, so a
        // link that keeps tipping over the high watermark should net lose
        // quality even if it also dips clear sometimes.
        let mut c = BandwidthController::new();
        for _ in 0..10 {
            c.sample(HIGH_WATERMARK + 1);
            c.sample(LOW_WATERMARK - 1);
        }
        assert!(c.scale() < 1.0);
    }
}
