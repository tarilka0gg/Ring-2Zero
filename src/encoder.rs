use crate::tile::Tile;
use std::collections::{HashMap, HashSet};

pub struct TileMerger {
    merge_gap: u32,
}

impl TileMerger {
    pub fn new(merge_gap: u32) -> Self {
        Self { merge_gap }
    }

    pub fn merge(&self, tiles: &[Tile], tiles_x: u32, tiles_y: u32, tile_width: u32, tile_height: u32, frame_width: u32, frame_height: u32) -> Vec<Tile> {
        let tile_set: HashSet<(u32, u32)> = tiles
            .iter()
            .map(|t| (t.x / tile_width, t.y / tile_height))
            .collect();

        // Build spatial index once: O(m) instead of O(m×n)
        let tile_quality_map: HashMap<(u32, u32), f32> = tiles
            .iter()
            .map(|t| ((t.x / tile_width, t.y / tile_height), t.quality))
            .collect();

        let mut run_cols: HashMap<(u32, u32), Vec<u32>> = HashMap::new();

        for tx in 0..tiles_x {
            for run in self.column_runs(&tile_set, tx, tiles_y) {
                run_cols.entry(run).or_default().push(tx);
            }
        }

        // Cap how many original grid tiles a single merged tile can span in
        // each direction. Without this, a full-screen refresh (all tiles
        // dirty, all contiguous) merges into one tile covering the entire
        // frame — its encoded size can exceed the WebRTC DataChannel's
        // message-size limit (observed: an 85 KB single-tile send tripped
        // "outbound packet larger than maximum message size" on a real
        // client). Chunking bounds the worst case regardless of how large a
        // contiguous dirty region is.
        const MAX_MERGE_TILES_X: u32 = 4;
        const MAX_MERGE_TILES_Y: u32 = 4;

        let mut merged = Vec::new();

        for ((ty_start, ty_end), mut cols) in run_cols {
            cols.sort_unstable();
            let mut group_start = 0;

            for i in 1..=cols.len() {
                if i == cols.len() || cols[i] != cols[i - 1] + 1 {
                    let tx_start = cols[group_start];
                    let tx_end = cols[i - 1];

                    let mut chunk_ty_start = ty_start;
                    while chunk_ty_start <= ty_end {
                        let chunk_ty_end = (chunk_ty_start + MAX_MERGE_TILES_Y - 1).min(ty_end);
                        let mut chunk_tx_start = tx_start;
                        while chunk_tx_start <= tx_end {
                            let chunk_tx_end = (chunk_tx_start + MAX_MERGE_TILES_X - 1).min(tx_end);

                            let x = chunk_tx_start * tile_width;
                            let y = chunk_ty_start * tile_height;
                            let width = ((chunk_tx_end + 1) * tile_width).min(frame_width) - x;
                            let height = ((chunk_ty_end + 1) * tile_height).min(frame_height) - y;

                            let quality = self.average_quality_fast(
                                &tile_quality_map, chunk_tx_start, chunk_tx_end, chunk_ty_start, chunk_ty_end,
                            );

                            merged.push(Tile::new(x, y, width, height, quality));
                            chunk_tx_start = chunk_tx_end + 1;
                        }
                        chunk_ty_start = chunk_ty_end + 1;
                    }

                    group_start = i;
                }
            }
        }

        merged
    }

    fn column_runs(&self, tile_set: &HashSet<(u32, u32)>, tx: u32, tiles_y: u32) -> Vec<(u32, u32)> {
        let mut runs = Vec::new();
        let mut start: Option<u32> = None;
        let mut last: Option<u32> = None;

        for ty in 0..tiles_y {
            if tile_set.contains(&(tx, ty)) {
                if start.is_none() {
                    start = Some(ty);
                }
                last = Some(ty);
            } else if let (Some(_), Some(l)) = (start, last) {
                if ty - l - 1 > self.merge_gap {
                    runs.push((start.unwrap(), l));
                    start = None;
                    last = None;
                }
            }
        }

        if let Some(s) = start {
            runs.push((s, last.unwrap()));
        }

        runs
    }

    // Spatial HashMap optimization: O(k) instead of O(m) where k = region size
    fn average_quality_fast(&self, tile_quality_map: &HashMap<(u32, u32), f32>, tx_start: u32, tx_end: u32, ty_start: u32, ty_end: u32) -> f32 {
        let mut sum = 0.0;
        let mut count = 0;

        for ty in ty_start..=ty_end {
            for tx in tx_start..=tx_end {
                if let Some(&quality) = tile_quality_map.get(&(tx, ty)) {
                    sum += quality;
                    count += 1;
                }
            }
        }

        if count == 0 {
            10.0
        } else {
            sum / count as f32
        }
    }
}

