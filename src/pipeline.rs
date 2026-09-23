//! Frame processing: diff → merge → prioritise → WebP encode. Synchronous
//! and CPU-bound; runs on its own thread (see `stream.rs`).

use crate::config::Config;
use crate::diff::DiffDetector;
use crate::encoder::TileMerger;
use crate::encoding_pool::{EncodingPool, EncodingTask};
use crate::frame::Frame;
use crate::tile::{Tile, TileMetadata};
use crate::tile_buffer_pool::TileBufferPool;
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

thread_local! {
    static TILE_BUFFER: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// One processed frame, ready for the transport.
pub struct EncodedFrame {
    /// Some((w, h)) when the resolution changed (or on the first frame): send a header first.
    pub header: Option<(u32, u32)>,
    /// Merged tiles, sorted highest-priority first.
    pub tiles: Vec<Tile>,
    /// WebP bytes, parallel to `tiles`.
    pub encoded: Vec<Vec<u8>>,
    /// Every original grid cell covered by `tiles` (for ACK-loss recovery).
    pub ack_indices: Vec<usize>,
    /// Tile-grid epoch these indices belong to.
    pub epoch: u64,
    pub produced_at: Instant,
    /// Wall time spent in `process`, in ms.
    pub process_ms: f64,
}

/// Tiles last sent at low ("dynamic") quality are re-armed this often, so a
/// tile that stopped moving right after a throttled send gets a
/// high-quality re-encode.
const FULL_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

pub struct Pipeline {
    diff_detector: DiffDetector,
    tile_merger: TileMerger,
    screen_size: Option<(u32, u32)>,
    last_full_refresh: Instant,
    tile_buffer_pool: TileBufferPool,
    encoding_pool: EncodingPool,
    epoch: u64,
    config: Config,
}

impl Pipeline {
    pub fn new(config: Config) -> Self {
        let diff_detector = DiffDetector::new(config.clone());
        let tile_merger = TileMerger::new(config.merge_gap);
        let tile_buffer_pool = TileBufferPool::new(120 * 68 * 4, 50);
        let encoding_pool = EncodingPool::new(num_cpus::get().max(4), tile_buffer_pool.clone());

        Self {
            diff_detector,
            tile_merger,
            screen_size: None,
            last_full_refresh: Instant::now(),
            tile_buffer_pool,
            encoding_pool,
            epoch: 0,
            config,
        }
    }

    /// Current tile-grid epoch; bumped on every resolution change after the first.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// ACK-loss recovery: force these grid cells to be re-sent. Indices from an
    /// older epoch describe a grid that no longer exists and are ignored (returns
    /// false); otherwise applies diff_detector.invalidate_tiles and returns true.
    pub fn invalidate(&mut self, epoch: u64, indices: &[usize]) -> bool {
        if epoch != self.epoch {
            return false;
        }

        self.diff_detector.invalidate_tiles(indices);
        true
    }

    /// Diffs, merges, prioritises and encodes one frame. Returns None when there
    /// is nothing to send (no changed tiles and no header).
    /// If the resolution changed but no tiles changed, returns Some with the
    /// header and empty tiles/encoded/ack_indices.
    pub fn process(&mut self, frame: &Frame) -> Option<EncodedFrame> {
        let (width, height) = (frame.width, frame.height);
        let t0 = Instant::now();

        let send_header = if self.screen_size != Some((width, height)) {
            if self.screen_size.is_some() {
                self.diff_detector.reset();
                self.epoch += 1;
            }
            self.screen_size = Some((width, height));
            self.last_full_refresh = Instant::now();
            Some((width, height))
        } else {
            None
        };

        let force_full_refresh = self.last_full_refresh.elapsed() >= FULL_REFRESH_INTERVAL;
        if force_full_refresh {
            self.diff_detector.invalidate_cache();
            self.last_full_refresh = Instant::now();
        }

        let (changed_tiles, _) = self.diff_detector.detect_changes(frame);

        if changed_tiles.is_empty() {
            if let Some(size) = send_header {
                return Some(EncodedFrame {
                    header: Some(size),
                    tiles: vec![],
                    encoded: vec![],
                    ack_indices: vec![],
                    epoch: self.epoch,
                    produced_at: Instant::now(),
                    process_ms: t0.elapsed().as_secs_f64() * 1000.0,
                });
            }
            return None;
        }

        let (tile_width, tile_height, tiles_y) =
            self.config.calculate_tile_dimensions(width, height);

        let merged_tiles = self.tile_merger.merge(
            &changed_tiles,
            self.config.tiles_x,
            tiles_y,
            tile_width,
            tile_height,
            width,
            height,
        );

        let mut tiles_with_data: Vec<(Tile, usize, f32)> = merged_tiles
            .iter()
            .map(|tile| {
                let tile_idx =
                    tile.representative_index(tile_width, tile_height, self.config.tiles_x);
                let metadata = self.diff_detector.get_metadata(tile_idx);
                let priority = Self::priority(tile, metadata, width, height, &self.config);
                (*tile, tile_idx, priority)
            })
            .collect();

        // A merged tile can span up to 4x4 original grid cells
        // (see encoder.rs's MAX_MERGE_TILES_X/Y), but tile_idx above
        // is only the ONE representative cell at its top-left
        // corner. The per-tile cache below is keyed on that single
        // cell's hash — using it to validate a cache entry whose
        // bytes cover the whole merged region would let a stale
        // encode (from a previous, differently-shaped merge sharing
        // the same representative cell) be resent while a *different*
        // covered cell has since changed. Restrict the cache
        // fast-path to genuinely single-cell tiles, where the
        // representative hash actually covers the whole tile.
        let is_single_cell = |tile: &Tile| {
            tile.is_single_cell(tile_width, tile_height, self.config.tiles_x, tiles_y)
        };

        tiles_with_data.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

        let sorted_tiles: Vec<Tile> = tiles_with_data.iter().map(|(t, _, _)| *t).collect();
        let sorted_tile_indices: Vec<usize> =
            tiles_with_data.iter().map(|(_, idx, _)| *idx).collect();

        // Every original grid cell covered by this frame's tiles, for
        // ACK-loss recovery: unlike sorted_tile_indices (one
        // representative cell per tile, used only for cache
        // bookkeeping below), invalidate_tiles needs ALL cells a lost
        // merged tile spanned — otherwise up to 15/16 of a lost 4x4
        // merged region would keep its stale content forever, since
        // only the one representative cell would ever get re-armed.
        let ack_indices: Vec<usize> = sorted_tiles
            .iter()
            .flat_map(|tile| {
                tile.covered_indices(tile_width, tile_height, self.config.tiles_x, tiles_y)
            })
            .collect();

        let tile_hashes: Vec<u64> = sorted_tile_indices
            .iter()
            .map(|&idx| self.diff_detector.get_current_hashes()[idx])
            .collect();

        let mut encoded = vec![Vec::new(); sorted_tiles.len()];
        let mut submitted_count = 0;

        for (i, tile) in sorted_tiles.iter().enumerate() {
            let tile_idx = sorted_tile_indices[i];

            let metadata = self.diff_detector.get_metadata(tile_idx);
            if is_single_cell(tile) && metadata.cached_hash == tile_hashes[i] {
                if let Some(ref cached) = metadata.cached_encoded {
                    encoded[i] = cached.clone();
                    continue;
                }
            }

            let mut tile_buffer = self.tile_buffer_pool.get();
            let tile_size = (tile.width * tile.height * 4) as usize;

            if tile_buffer.len() != tile_size {
                tile_buffer.clear();
                tile_buffer.resize(tile_size, 0);
            }

            crate::tile_extract::extract_tile(
                &frame.rgba,
                &mut tile_buffer,
                tile.x,
                tile.y,
                tile.width,
                tile.height,
                width,
            );

            let task = EncodingTask {
                tile: *tile,
                tile_data: tile_buffer,
                tile_idx,
            };

            let _ = self.encoding_pool.submit(task);
            submitted_count += 1;
        }

        let encoded_results = self.encoding_pool.collect_results(submitted_count);

        let tile_idx_to_pos: HashMap<usize, usize> = sorted_tile_indices
            .iter()
            .enumerate()
            .map(|(pos, &idx)| (idx, pos))
            .collect();

        for result in encoded_results {
            if let Some(&pos) = tile_idx_to_pos.get(&result.tile_idx) {
                encoded[pos] = result.data;
            } else {
                log::warn!("Received result for unknown tile_idx={}", result.tile_idx);
            }
        }

        let mut missing_tiles = Vec::new();
        for (i, data) in encoded.iter().enumerate() {
            if data.is_empty() {
                missing_tiles.push(i);
            }
        }

        for i in missing_tiles {
            let tile_idx = sorted_tile_indices[i];
            log::error!(
                "Tile {} was not encoded (worker panic or channel overflow)",
                tile_idx
            );

            let tile = &sorted_tiles[i];
            let fallback_encoded = TILE_BUFFER.with(|buf| {
                let mut tile_buffer = buf.borrow_mut();

                let max_height = height.saturating_sub(tile.y).min(tile.height);
                let max_width = width.saturating_sub(tile.x).min(tile.width);

                if max_width == 0 || max_height == 0 {
                    return Vec::new();
                }

                let tile_size = (max_width * max_height * 4) as usize;
                tile_buffer.clear();
                tile_buffer.resize(tile_size, 0);

                crate::tile_extract::extract_tile(
                    &frame.rgba,
                    &mut tile_buffer,
                    tile.x,
                    tile.y,
                    max_width,
                    max_height,
                    width,
                );

                fast_webp::encode_rgba(
                    &tile_buffer,
                    max_width,
                    max_height,
                    fast_webp::WebpOptions {
                        quality: tile.quality,
                        ..Default::default()
                    },
                )
                .unwrap_or_else(|_| Vec::new())
            });

            encoded[i] = fallback_encoded;
        }

        let tile_metadata = self.diff_detector.get_all_metadata_mut();
        for (i, &tile_idx) in sorted_tile_indices.iter().enumerate() {
            if !is_single_cell(&sorted_tiles[i]) {
                continue;
            }
            let metadata = &mut tile_metadata[tile_idx];
            if !encoded[i].is_empty() {
                metadata.cached_encoded = Some(encoded[i].clone());
                metadata.cached_hash = tile_hashes[i];
            }
        }

        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;

        Some(EncodedFrame {
            header: send_header,
            tiles: sorted_tiles,
            encoded,
            ack_indices,
            epoch: self.epoch,
            produced_at: Instant::now(),
            process_ms: elapsed_ms,
        })
    }

    fn priority(
        tile: &Tile,
        metadata: &TileMetadata,
        width: u32,
        height: u32,
        config: &Config,
    ) -> f32 {
        let frequency_score = metadata.update_frequency();
        let change_speed = (metadata.last_hash_diff.count_ones() as f32) / 64.0;
        let center_x = width / 2;
        let center_y = height / 2;
        let distance = tile.distance_from_center(center_x, center_y) as f32;
        let max_distance = ((width * width + height * height) / 4) as f32;
        let center_score = 1.0 - (distance / max_distance).sqrt();

        frequency_score * config.priority_frequency_weight
            + change_speed * config.priority_speed_weight
            + center_score * config.priority_center_weight
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        Config {
            tiles_x: 4,
            merge_gap: 0,
            ..Config::default()
        }
    }

    fn solid(w: u32, h: u32, v: u8) -> Frame {
        Frame {
            rgba: vec![v; (w * h * 4) as usize],
            width: w,
            height: h,
            damage_regions: vec![],
        }
    }

    #[test]
    fn process_first_frame_returns_header_and_tiles() {
        let config = test_config();
        let mut pipeline = Pipeline::new(config);
        let frame = solid(64, 36, 128);

        let result = pipeline.process(&frame).unwrap();

        assert!(result.header.is_some());
        assert_eq!(result.header.unwrap(), (64, 36));
        assert!(!result.tiles.is_empty());
        assert_eq!(result.tiles.len(), result.encoded.len());
        assert!(!result.encoded.is_empty());
        for encoded_tile in &result.encoded {
            assert!(!encoded_tile.is_empty());
        }
    }

    #[test]
    fn process_identical_frame_returns_none() {
        let config = test_config();
        let mut pipeline = Pipeline::new(config);
        let frame = solid(64, 36, 128);

        // First frame
        let _ = pipeline.process(&frame);

        // Second identical frame
        let result = pipeline.process(&frame);
        assert!(result.is_none());
    }

    #[test]
    fn process_different_size_frame_bumps_epoch() {
        let config = test_config();
        let mut pipeline = Pipeline::new(config);
        let frame1 = solid(64, 36, 128);
        let frame2 = solid(128, 72, 128);

        assert!(pipeline.process(&frame1).is_some());
        assert_eq!(pipeline.epoch(), 0);

        let result2 = pipeline.process(&frame2).unwrap();
        assert_eq!(pipeline.epoch(), 1);
        assert!(result2.header.is_some());
        assert_eq!(result2.header.unwrap().0, 128);
        assert_eq!(result2.header.unwrap().1, 72);
    }

    #[test]
    fn invalidate_with_current_epoch_returns_true() {
        let config = test_config();
        let mut pipeline = Pipeline::new(config);
        let frame = solid(64, 36, 128);

        let _ = pipeline.process(&frame);
        let current_epoch = pipeline.epoch();

        let result = pipeline.invalidate(current_epoch, &[0, 1]);
        assert!(result);
    }

    #[test]
    fn invalidate_with_stale_epoch_returns_false() {
        let mut pipeline = Pipeline::new(test_config());
        let _ = pipeline.process(&solid(64, 36, 128));
        let _ = pipeline.process(&solid(128, 72, 128));
        assert!(
            !pipeline.invalidate(0, &[0, 1]),
            "cells from the old 64x36 grid"
        );
    }

    #[test]
    fn invalidated_cells_are_resent_on_an_unchanged_frame() {
        let mut pipeline = Pipeline::new(test_config());
        let frame = solid(64, 36, 128);
        let _ = pipeline.process(&frame);
        assert!(pipeline.process(&frame).is_none());
        assert!(pipeline.invalidate(pipeline.epoch(), &[0]));
        let out = pipeline.process(&frame).expect("cell 0 must be re-sent");
        assert!(out.ack_indices.contains(&0));
        assert_eq!(out.header, None);
    }

    #[test]
    fn priority_is_higher_near_screen_center() {
        let config = Config::default();
        let metadata = TileMetadata::default();
        let center_tile = Tile::new(940, 520, 40, 40, 5.0); // near the center of 1920x1080
        let corner_tile = Tile::new(0, 0, 40, 40, 5.0);

        let center = Pipeline::priority(&center_tile, &metadata, 1920, 1080, &config);
        let corner = Pipeline::priority(&corner_tile, &metadata, 1920, 1080, &config);
        assert!(
            center > corner,
            "a tile near the center should score higher than one at the corner"
        );
    }

    #[test]
    fn priority_increases_with_change_frequency() {
        let config = Config::default();
        let tile = Tile::new(0, 0, 40, 40, 5.0);

        let mut quiet = TileMetadata::default();
        for _ in 0..10 {
            quiet.change_history.push(false);
        }
        let mut busy = TileMetadata::default();
        for _ in 0..10 {
            busy.change_history.push(true);
        }

        let quiet_priority = Pipeline::priority(&tile, &quiet, 1920, 1080, &config);
        let busy_priority = Pipeline::priority(&tile, &busy, 1920, 1080, &config);
        assert!(
            busy_priority > quiet_priority,
            "a tile that keeps changing should score higher than one that never does"
        );
    }
}
