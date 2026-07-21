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

#[cfg(test)]
mod tests {
    use super::*;

    const TILE_W: u32 = 10;
    const TILE_H: u32 = 10;

    #[test]
    fn single_tile_passes_through_unchanged() {
        let merger = TileMerger::new(0);
        let tiles = [Tile::new(10, 10, TILE_W, TILE_H, 5.0)]; // grid cell (1,1)
        let merged = merger.merge(&tiles, 4, 4, TILE_W, TILE_H, 40, 40);
        assert_eq!(merged.len(), 1);
        assert_eq!((merged[0].x, merged[0].y, merged[0].width, merged[0].height), (10, 10, 10, 10));
        assert_eq!(merged[0].quality, 5.0);
    }

    #[test]
    fn horizontally_adjacent_tiles_merge_into_one() {
        let merger = TileMerger::new(0);
        let tiles = [
            Tile::new(10, 10, TILE_W, TILE_H, 4.0), // cell (1,1)
            Tile::new(20, 10, TILE_W, TILE_H, 8.0), // cell (2,1), adjacent
        ];
        let merged = merger.merge(&tiles, 4, 4, TILE_W, TILE_H, 40, 40);
        assert_eq!(merged.len(), 1);
        let t = &merged[0];
        assert_eq!((t.x, t.y, t.width, t.height), (10, 10, 20, 10));
        assert_eq!(t.quality, 6.0); // average of the two source tiles
    }

    #[test]
    fn non_adjacent_tiles_stay_separate() {
        let merger = TileMerger::new(0);
        let tiles = [
            Tile::new(0, 0, TILE_W, TILE_H, 5.0),   // cell (0,0)
            Tile::new(30, 0, TILE_W, TILE_H, 5.0),  // cell (3,0) — two empty columns between
        ];
        let merged = merger.merge(&tiles, 4, 4, TILE_W, TILE_H, 40, 40);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merge_gap_tolerates_one_missing_row_even_at_gap_zero() {
        // merge_gap's tolerance check (`ty - last - 1 > merge_gap`) is 0 for
        // exactly one missing row, so even merge_gap=0 bridges a single gap
        // row — documenting actual behavior, not a requirement.
        let merger = TileMerger::new(0);
        let tiles = [
            Tile::new(0, 0, TILE_W, TILE_H, 5.0),  // cell (0,0)
            Tile::new(0, 20, TILE_W, TILE_H, 5.0), // cell (0,2) — row 1 empty
        ];
        let merged = merger.merge(&tiles, 1, 3, TILE_W, TILE_H, 10, 30);
        assert_eq!(merged.len(), 1);
        assert_eq!((merged[0].y, merged[0].height), (0, 30));
    }

    #[test]
    fn merge_gap_zero_does_not_bridge_a_two_row_gap() {
        let merger = TileMerger::new(0);
        let tiles = [
            Tile::new(0, 0, TILE_W, TILE_H, 5.0),  // cell (0,0)
            Tile::new(0, 30, TILE_W, TILE_H, 5.0), // cell (0,3) — rows 1,2 empty
        ];
        let merged = merger.merge(&tiles, 1, 4, TILE_W, TILE_H, 10, 40);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merge_gap_one_bridges_a_two_row_gap() {
        let merger = TileMerger::new(1);
        let tiles = [
            Tile::new(0, 0, TILE_W, TILE_H, 5.0),  // cell (0,0)
            Tile::new(0, 30, TILE_W, TILE_H, 5.0), // cell (0,3) — rows 1,2 empty
        ];
        let merged = merger.merge(&tiles, 1, 4, TILE_W, TILE_H, 10, 40);
        assert_eq!(merged.len(), 1);
        assert_eq!((merged[0].y, merged[0].height), (0, 40));
    }

    #[test]
    fn a_full_screen_refresh_is_capped_to_4x4_cell_chunks() {
        // Without the MAX_MERGE_TILES_X/Y cap, every tile dirty would merge
        // into one giant rectangle that can exceed the DataChannel's
        // message-size limit (see the cap's doc comment in TileMerger::merge).
        let merger = TileMerger::new(0);
        let tiles_x = 8;
        let tiles_y = 8;
        let mut tiles = Vec::new();
        for ty in 0..tiles_y {
            for tx in 0..tiles_x {
                tiles.push(Tile::new(tx * TILE_W, ty * TILE_H, TILE_W, TILE_H, 5.0));
            }
        }
        let merged = merger.merge(&tiles, tiles_x, tiles_y, TILE_W, TILE_H, tiles_x * TILE_W, tiles_y * TILE_H);
        // An 8x8 grid entirely dirty must chunk into 2x2 = 4 pieces of at
        // most 4x4 cells each, never one single 8x8 blob.
        assert_eq!(merged.len(), 4);
        for t in &merged {
            assert!(t.width <= 4 * TILE_W, "chunk wider than the 4-cell cap: {}", t.width);
            assert!(t.height <= 4 * TILE_H, "chunk taller than the 4-cell cap: {}", t.height);
        }
    }

    #[test]
    fn empty_input_produces_no_merged_tiles() {
        let merger = TileMerger::new(0);
        let merged = merger.merge(&[], 4, 4, TILE_W, TILE_H, 40, 40);
        assert!(merged.is_empty());
    }
}

