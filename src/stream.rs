use crate::config::Config;
use crate::diff::DiffDetector;
use crate::encoder::{TileEncoder, TileMerger};
use crate::error::Result;
use crate::frame::Frame;
use crate::tile::Tile;

use std::sync::mpsc;
use std::time::{Duration, Instant};
use std::sync::Arc;
use std::cell::RefCell;
use webrtc::data_channel::RTCDataChannel;
use bytes::Bytes;

thread_local! {
    static TILE_BUFFER: RefCell<Vec<u8>> = RefCell::new(Vec::new());
}

pub struct StreamServer {
    config: Config,
    frame_data_buf: std::sync::Mutex<Vec<u8>>,
    tile_buffer_buf: std::sync::Mutex<Vec<u8>>,
}

impl StreamServer {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            frame_data_buf: std::sync::Mutex::new(Vec::with_capacity(8_000)),
            tile_buffer_buf: std::sync::Mutex::new(Vec::with_capacity(1 << 16)),
        }
    }

    fn receive_frame(frame_rx: &mpsc::Receiver<Frame>, deadline: Instant) -> Option<Frame> {
        loop {
            let now = Instant::now();
            if now >= deadline {
                return None;
            }

            match frame_rx.recv_timeout(deadline - now) {
                Ok(frame) => return Some(frame),
                Err(mpsc::RecvTimeoutError::Timeout) => return None,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    eprintln!("Capture нитка відключилась");
                    return None;
                }
            }
        }
    }

    fn sleep_until(deadline: Instant) {
        let now = Instant::now();
        if now < deadline {
            std::thread::sleep(deadline - now);
        }
    }

    // Async версія з pipeline для WebRTC DataChannel
    pub async fn handle_client_async(
        &self,
        data_channel: Arc<RTCDataChannel>,
        frame_rx: mpsc::Receiver<Frame>,
    ) -> Result<()> {
        println!("Клієнт підключився!");

        // Channel для pipeline: Process → Send
        // Передаємо: (header_option, tiles, encoded, timestamp, elapsed_ms)
        // Buffer=0 для мінімальної латентності (rendezvous channel)
        let (encode_tx, mut encode_rx) = tokio::sync::mpsc::channel::<(Option<(u32, u32)>, Vec<Tile>, Vec<Vec<u8>>, Instant, f64)>(0);

        let config = self.config.clone();
        let _tile_encoder = TileEncoder::new(config.clone());

        // Thread: Capture → Diff → Encode → Channel
        let process_handle = std::thread::spawn(move || {
            let frame_duration = config.frame_duration();
            let mut diff_detector = DiffDetector::new(config.clone());
            let tile_merger = TileMerger::new(config.merge_gap);
            let mut screen_size: Option<(u32, u32)> = None;

            // Create tile buffer pool
            // With tiles_x=20: 96×54px = 20,736 bytes
            // With tiles_x=16: 120×68px = 32,640 bytes (worst case for common configs)
            // Allocate for worst case to avoid reallocations
            let tile_buffer_pool = crate::tile_buffer_pool::TileBufferPool::new(
                120 * 68 * 4,  // Max tile size for tiles_x=16 on 1920×1080
                50             // Initial buffer count
            );

            // Create encoding pool with worker threads (passes buffer_pool for reuse)
            let encoding_pool = crate::encoding_pool::EncodingPool::new(
                num_cpus::get().max(4),  // Use all CPU cores, minimum 4
                tile_buffer_pool.clone() // Workers return buffers after encoding
            );

            loop {
                let deadline = Instant::now() + frame_duration;

                let frame = match Self::receive_frame(&frame_rx, deadline) {
                    Some(f) => f,
                    None => continue,
                };

                let (width, height) = (frame.width, frame.height);
                let t0 = Instant::now();

                // Перевіряємо чи змінився розмір
                let send_header = if screen_size.map_or(true, |s| s != (width, height)) {
                    diff_detector.reset();
                    screen_size = Some((width, height));
                    Some((width, height))
                } else {
                    None
                };

                let (changed_tiles, tile_indices) = diff_detector.detect_changes(&frame);

                if changed_tiles.is_empty() {
                    // Якщо треба відправити header але немає тайлів - все одно відправляємо
                    if let Some(size) = send_header {
                        if encode_tx.blocking_send((Some(size), vec![], vec![], Instant::now(), 0.0)).is_err() {
                            break;
                        }
                    }
                    Self::sleep_until(deadline);
                    continue;
                }

                let (tile_width, tile_height, tiles_y) = config.calculate_tile_dimensions(width, height);

                let merged_tiles = tile_merger.merge(
                    &changed_tiles,
                    config.tiles_x,
                    tiles_y,
                    tile_width,
                    tile_height,
                    width,
                    height,
                );

                // Keep original tile_idx with tile during sorting
                let mut tiles_with_data: Vec<(Tile, usize, f32)> = merged_tiles
                    .iter()
                    .enumerate()
                    .map(|(enum_idx, tile)| {
                        let tile_idx = tile_indices[enum_idx];
                        let metadata = diff_detector.get_metadata(tile_idx);
                        let priority = Self::calculate_priority_static(tile, metadata, width, height, &config);
                        (*tile, tile_idx, priority)  // Store original tile_idx, not enum index
                    })
                    .collect();

                tiles_with_data.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

                let sorted_tiles: Vec<Tile> = tiles_with_data.iter().map(|(t, _, _)| *t).collect();
                let sorted_tile_indices: Vec<usize> = tiles_with_data.iter().map(|(_, idx, _)| *idx).collect();

                // Optimization #3: Async encoding через pool
                let tile_hashes: Vec<u64> = sorted_tile_indices.iter()
                    .map(|&idx| diff_detector.get_current_hashes()[idx])
                    .collect();

                // Clone cache data before parallel work to avoid borrowing issues
                let cached_data: Vec<Option<Vec<u8>>> = sorted_tile_indices.iter()
                    .enumerate()
                    .map(|(i, &idx)| {
                        let metadata = diff_detector.get_metadata(idx);
                        if metadata.cached_hash == tile_hashes[i] {
                            // Cache hit - clone Vec
                            metadata.cached_encoded.clone()
                        } else {
                            None
                        }
                    })
                    .collect();

                // Pre-fill encoded vec with cache hits and track submissions
                let mut encoded = vec![Vec::new(); sorted_tiles.len()];
                let mut submitted_count = 0;

                // Submit encoding tasks to pool
                for (i, tile) in sorted_tiles.iter().enumerate() {
                    let tile_idx = sorted_tile_indices[i];
                    let _tile_hash = tile_hashes[i];

                    // Check cache first (using pre-cloned data)
                    if let Some(ref cached) = cached_data[i] {
                        // Cache hit - use cached data directly (already converted to Vec)
                        encoded[i] = cached.clone();
                        continue;
                    }

                    // Get buffer from pool (reusable)
                    let mut tile_buffer = tile_buffer_pool.get();
                    let tile_size = (tile.width * tile.height * 4) as usize;

                    // Resize if needed (rare case for non-standard tile sizes)
                    if tile_buffer.len() != tile_size {
                        tile_buffer.clear();
                        tile_buffer.resize(tile_size, 0);
                    }

                    // Extract tile data with SIMD optimization
                    crate::tile_extract::extract_tile(
                        &frame.rgba,
                        &mut tile_buffer,
                        tile.x,
                        tile.y,
                        tile.width,
                        tile.height,
                        width,
                    );

                    // Submit to encoding pool
                    let task = crate::encoding_pool::EncodingTask {
                        tile: *tile,
                        tile_data: tile_buffer, // Pool buffer (will not be returned)
                        tile_idx,
                    };

                    let _ = encoding_pool.submit(task);
                    submitted_count += 1;
                }

                // Collect only submitted tasks (not cache hits)
                let encoded_results = encoding_pool.collect_results(submitted_count);

                // Fill in non-cached results
                for result in encoded_results {
                    // Find position in sorted_tile_indices (correct after sorting)
                    if let Some(pos) = sorted_tile_indices.iter().position(|&idx| idx == result.tile_idx) {
                        encoded[pos] = result.data;
                    } else {
                        eprintln!("⚠️  WARNING: Received result for unknown tile_idx={}", result.tile_idx);
                    }
                }

                // Verify all tiles were encoded and collect missing indices
                let mut missing_tiles = Vec::new();
                for (i, data) in encoded.iter().enumerate() {
                    if data.is_empty() {
                        missing_tiles.push(i);
                    }
                }

                // Re-encode missing tiles synchronously as fallback
                for i in missing_tiles {
                    let tile_idx = sorted_tile_indices[i];
                    eprintln!("❌ ERROR: Tile {} was not encoded (worker panic or channel overflow)", tile_idx);

                    let tile = &sorted_tiles[i];
                    let fallback_encoded = TILE_BUFFER.with(|buf| {
                        let mut tile_buffer = buf.borrow_mut();

                        // Bounds check: clamp tile dimensions to frame boundaries
                        let max_height = height.saturating_sub(tile.y).min(tile.height);
                        let max_width = width.saturating_sub(tile.x).min(tile.width);
                        let tile_size = (max_width * max_height * 4) as usize;

                        tile_buffer.clear();
                        tile_buffer.resize(tile_size, 0);

                        // Extract tile data with SIMD optimization (with bounds checking)
                        crate::tile_extract::extract_tile(
                            &frame.rgba,
                            &mut tile_buffer,
                            tile.x,
                            tile.y,
                            max_width,
                            max_height,
                            width,
                        );

                        // Encode with actual extracted dimensions, not original tile dimensions
                        fast_webp::encode_rgba(
                            &tile_buffer,
                            max_width,
                            max_height,
                            fast_webp::WebpOptions {
                                quality: tile.quality,
                                ..Default::default()
                            },
                        ).unwrap_or_else(|e| {
                            eprintln!("⚠️  WebP fallback encoding error at tile ({}, {}), {}×{}: {:?}",
                                      tile.x, tile.y, max_width, max_height, e);
                            eprintln!("    Attempting second fallback with quality 50...");

                            // Second fallback: try with lower quality
                            fast_webp::encode_rgba(
                                &tile_buffer,
                                max_width,
                                max_height,
                                fast_webp::WebpOptions {
                                    quality: 50.0,
                                    ..Default::default()
                                },
                            ).unwrap_or_else(|e2| {
                                eprintln!("❌ CRITICAL: All fallback attempts failed: {:?}", e2);
                                Vec::new()
                            })
                        })
                    });

                    encoded[i] = fallback_encoded;
                }

                // Update cache (get mutable reference after parallel work is done)
                let tile_metadata = diff_detector.get_all_metadata_mut();
                for (i, &tile_idx) in sorted_tile_indices.iter().enumerate() {
                    let metadata = &mut tile_metadata[tile_idx];
                    if !encoded[i].is_empty() {
                        // Store encoded tile in cache
                        metadata.cached_encoded = Some(encoded[i].clone());
                        metadata.cached_hash = tile_hashes[i];
                    }
                }

                let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
                let timestamp = Instant::now();

                // Відправляємо в channel з header якщо потрібно
                if encode_tx.blocking_send((send_header, sorted_tiles, encoded, timestamp, elapsed_ms)).is_err() {
                    break;
                }

                Self::sleep_until(deadline);
            }
        });

        // Tokio task: Channel → Async Send
        let mut frame_count = 0u64;
        let mut avg_ms = 0.0f64;

        while let Some((header, tiles, encoded, timestamp, elapsed_ms)) = encode_rx.recv().await {
            // Відправляємо header якщо потрібно
            if let Some((width, height)) = header {
                self.send_header_async(&data_channel, width, height).await?;
            }

            // Відправляємо тайли якщо є
            if !tiles.is_empty() {
                let queue_time = timestamp.elapsed();
                let send_start = Instant::now();
                self.send_tiles_async(&data_channel, &tiles, &encoded).await?;
                let send_time = send_start.elapsed();

                let stats = FrameStats::new(tiles.len(), &encoded, Duration::ZERO);
                frame_count += 1;
                avg_ms += (elapsed_ms - avg_ms) / frame_count as f64;

                println!(
                    "{} тайлів / {:.1} кбіт / {:.1} мс / сер. {:.1} мс / queue: {:.1}ms / send: {:.1}ms",
                    stats.tiles_sent, stats.total_kbits, elapsed_ms, avg_ms,
                    queue_time.as_secs_f64() * 1000.0,
                    send_time.as_secs_f64() * 1000.0
                );
            }
        }

        let _ = process_handle.join();
        Ok(())
    }

    // Static версія calculate_priority для використання в thread
    fn calculate_priority_static(tile: &Tile, metadata: &crate::tile::TileMetadata, width: u32, height: u32, config: &Config) -> f32 {
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

    async fn send_header_async(
        &self,
        data_channel: &Arc<RTCDataChannel>,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let mut header = Vec::with_capacity(6);
        header.extend_from_slice(&0xFFFFu16.to_le_bytes());
        header.extend_from_slice(&(width as u16).to_le_bytes());
        header.extend_from_slice(&(height as u16).to_le_bytes());
        data_channel.send(&Bytes::from(header)).await?;
        Ok(())
    }

    async fn send_tiles_async(
        &self,
        data_channel: &Arc<RTCDataChannel>,
        tiles: &[Tile],
        encoded: &[Vec<u8>],
    ) -> Result<()> {
        // Зменшений розмір пакету для нижчої латентності
        const MAX_PACKET_SIZE: usize = 8_000; // 8KB - баланс між латентністю та overhead

        // Clone buffers into local variables to avoid holding MutexGuard across await
        let mut frame_data = {
            let mut buf = self.frame_data_buf.lock().unwrap();
            buf.clear();
            buf.clone()
        };

        let mut tile_buffer = {
            let mut buf = self.tile_buffer_buf.lock().unwrap();
            buf.clear();
            buf.clone()
        };

        for (tile, webp) in tiles.iter().zip(encoded.iter()) {
            tile_buffer.clear();
            tile_buffer.extend_from_slice(&(tile.x as u16).to_le_bytes());
            tile_buffer.extend_from_slice(&(tile.y as u16).to_le_bytes());
            tile_buffer.extend_from_slice(&(tile.width as u16).to_le_bytes());
            tile_buffer.extend_from_slice(&(tile.height as u16).to_le_bytes());
            tile_buffer.extend_from_slice(webp);

            let tile_size = 4 + tile_buffer.len(); // 4 bytes для довжини + дані тайла

            // Якщо додавання цього тайла перевищить ліміт - відправляємо поточний пакет НЕГАЙНО
            if !frame_data.is_empty() && frame_data.len() + tile_size > MAX_PACKET_SIZE {
                data_channel.send(&Bytes::from(frame_data.clone())).await?;
                frame_data.clear();
            }

            frame_data.extend_from_slice(&(tile_buffer.len() as u32).to_le_bytes());
            frame_data.extend_from_slice(&tile_buffer);
        }

        // Відправляємо залишок
        if !frame_data.is_empty() {
            data_channel.send(&Bytes::from(frame_data.clone())).await?;
        }

        Ok(())
    }
}

struct FrameStats {
    tiles_sent: usize,
    total_kbits: f64,
}

impl FrameStats {
    fn new(tiles_sent: usize, encoded: &[Vec<u8>], _elapsed: Duration) -> Self {
        let total_kbits = encoded.iter().map(|e| e.len()).sum::<usize>() as f64 * 8.0 / 1000.0;

        Self {
            tiles_sent,
            total_kbits,
        }
    }
}
