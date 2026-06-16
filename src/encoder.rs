use crate::tile::Tile;
use rayon::prelude::*;
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

        let mut merged = Vec::new();

        for ((ty_start, ty_end), mut cols) in run_cols {
            cols.sort_unstable();
            let mut group_start = 0;

            for i in 1..=cols.len() {
                if i == cols.len() || cols[i] != cols[i - 1] + 1 {
                    let tx_start = cols[group_start];
                    let tx_end = cols[i - 1];

                    let x = tx_start * tile_width;
                    let y = ty_start * tile_height;
                    let width = ((tx_end + 1) * tile_width).min(frame_width) - x;
                    let height = ((ty_end + 1) * tile_height).min(frame_height) - y;

                    let quality = self.average_quality_fast(&tile_quality_map, tx_start, tx_end, ty_start, ty_end);

                    merged.push(Tile::new(x, y, width, height, quality));
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

    fn average_quality(&self, tiles: &[Tile], tx_start: u32, tx_end: u32, ty_start: u32, ty_end: u32, tile_width: u32, tile_height: u32) -> f32 {
        let qualities: Vec<f32> = tiles
            .iter()
            .filter(|t| {
                let tx = t.x / tile_width;
                let ty = t.y / tile_height;
                tx >= tx_start && tx <= tx_end && ty >= ty_start && ty <= ty_end
            })
            .map(|t| t.quality)
            .collect();

        if qualities.is_empty() {
            10.0
        } else {
            qualities.iter().sum::<f32>() / qualities.len() as f32
        }
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

pub struct TileEncoder {
    config: crate::config::Config,
}

impl TileEncoder {
    pub fn new(config: crate::config::Config) -> Self {
        Self { config }
    }

    // Optimization #3: Encode with cache support
    pub fn encode_tiles_with_cache(
        &self,
        tiles: &[Tile],
        tile_hashes: &[u64],
        tile_metadata: &mut [crate::tile::TileMetadata],
        tile_indices: &[usize],
        frame_data: &[u8],
        frame_width: u32,
    ) -> Vec<Vec<u8>> {
        thread_local! {
            static TILE_BUF: std::cell::RefCell<Vec<u8>> = std::cell::RefCell::new(Vec::new());
        }

        tiles
            .par_iter()
            .enumerate()
            .map(|(i, tile)| {
                let tile_idx = tile_indices[i];
                let tile_hash = tile_hashes[tile_idx];
                let metadata = &tile_metadata[tile_idx];

                // Перевіряємо кеш
                if let Some(cached) = &metadata.cached_encoded {
                    if metadata.cached_hash == tile_hash {
                        // Cache hit! Пропускаємо encoding
                        return cached.clone();
                    }
                }

                // Cache miss - кодуємо tile
                TILE_BUF.with(|cell| {
                    let mut buf = cell.borrow_mut();
                    let tile_size = (tile.width * tile.height * 4) as usize;
                    buf.resize(tile_size, 0);

                    if tile.width == frame_width {
                        let src_offset = (tile.y * frame_width * 4) as usize;
                        buf.copy_from_slice(&frame_data[src_offset..src_offset + tile_size]);
                    } else {
                        for row in 0..tile.height {
                            let src_offset = (((tile.y + row) * frame_width + tile.x) * 4) as usize;
                            let dst_offset = (row * tile.width * 4) as usize;
                            let len = (tile.width * 4) as usize;
                            buf[dst_offset..dst_offset + len]
                                .copy_from_slice(&frame_data[src_offset..src_offset + len]);
                        }
                    }

                    webp::Encoder::from_rgba(&buf, tile.width, tile.height)
                        .encode(tile.quality)
                        .to_vec()
                })
            })
            .collect()
    }

    // Original method (без кешування) для backward compatibility
    pub fn encode_tiles(&self, tiles: &[Tile], frame_data: &[u8], frame_width: u32, _frame_height: u32) -> Vec<Vec<u8>> {
        thread_local! {
            static TILE_BUF: std::cell::RefCell<Vec<u8>> = std::cell::RefCell::new(Vec::new());
        }

        tiles
            .par_iter()
            .map(|tile| {
                TILE_BUF.with(|cell| {
                    let mut buf = cell.borrow_mut();
                    let tile_size = (tile.width * tile.height * 4) as usize;
                    buf.resize(tile_size, 0);

                    // Оптимізація: якщо тайл повної ширини, копіюємо одним блоком
                    if tile.width == frame_width {
                        let src_offset = (tile.y * frame_width * 4) as usize;
                        buf.copy_from_slice(&frame_data[src_offset..src_offset + tile_size]);
                    } else {
                        // Інакше копіюємо рядок за рядком
                        for row in 0..tile.height {
                            let src_offset = (((tile.y + row) * frame_width + tile.x) * 4) as usize;
                            let dst_offset = (row * tile.width * 4) as usize;
                            let len = (tile.width * 4) as usize;
                            buf[dst_offset..dst_offset + len]
                                .copy_from_slice(&frame_data[src_offset..src_offset + len]);
                        }
                    }

                    webp::Encoder::from_rgba(&buf, tile.width, tile.height)
                        .encode(tile.quality)
                        .to_vec()
                })
            })
            .collect()
    }
}
