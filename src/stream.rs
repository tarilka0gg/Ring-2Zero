use crate::config::Config;
use crate::diff::DiffDetector;
use crate::encoder::TileMerger;
use crate::error::Result;
use crate::frame::Frame;
use crate::tile::Tile;

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::cell::RefCell;
use webrtc::data_channel::RTCDataChannel;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
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

    pub async fn handle_client_async(
        &self,
        data_channel: Arc<RTCDataChannel>,
        frame_rx: mpsc::Receiver<Frame>,
    ) -> Result<()> {
        println!("Клієнт підключився!");

        // --- ACK system setup ---
        // Tiles lost in transit (ACK timeout) are pushed here by the async task
        // and consumed by the processing thread to force re-send.
        let stale_tiles: Arc<std::sync::Mutex<Vec<usize>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let stale_tiles_thread = Arc::clone(&stale_tiles);

        let (ack_tx, mut ack_rx) = tokio::sync::mpsc::unbounded_channel::<u32>();
        let ack_tx_cb = ack_tx.clone();
        data_channel.on_message(Box::new(move |msg: DataChannelMessage| {
            let ack_tx = ack_tx_cb.clone();
            Box::pin(async move {
                // Client ACKs are 4-byte little-endian u32 sequence numbers
                if !msg.is_string && msg.data.len() == 4 {
                    let bytes = [msg.data[0], msg.data[1], msg.data[2], msg.data[3]];
                    let _ = ack_tx.send(u32::from_le_bytes(bytes));
                }
            })
        }));

        // encode channel: (header, tiles, encoded, tile_indices, timestamp, elapsed_ms)
        let (encode_tx, mut encode_rx) = tokio::sync::mpsc::channel::<(
            Option<(u32, u32)>,
            Vec<Tile>,
            Vec<Vec<u8>>,
            Vec<usize>,
            Instant,
            f64,
        )>(4);

        let config = self.config.clone();
        let process_handle = std::thread::spawn(move || {
            let frame_duration = config.frame_duration();
            let mut diff_detector = DiffDetector::new(config.clone());
            let tile_merger = TileMerger::new(config.merge_gap);
            let mut screen_size: Option<(u32, u32)> = None;
            let mut last_full_refresh = Instant::now();
            let full_refresh_interval = Duration::from_secs(1);

            let tile_buffer_pool = crate::tile_buffer_pool::TileBufferPool::new(
                120 * 68 * 4,
                50,
            );

            let encoding_pool = crate::encoding_pool::EncodingPool::new(
                num_cpus::get().max(4),
                tile_buffer_pool.clone(),
            );

            loop {
                let deadline = Instant::now() + frame_duration;

                // Apply stale tile invalidations from ACK timeout before diff
                {
                    let mut stale = stale_tiles_thread.lock().unwrap();
                    if !stale.is_empty() {
                        let indices: Vec<usize> = std::mem::take(&mut *stale);
                        diff_detector.invalidate_tiles(&indices);
                        println!("Re-queued {} tiles lost in transit", indices.len());
                    }
                }

                let frame = match Self::receive_frame(&frame_rx, deadline) {
                    Some(f) => f,
                    None => continue,
                };

                let (width, height) = (frame.width, frame.height);
                let t0 = Instant::now();

                let send_header = if screen_size.map_or(true, |s| s != (width, height)) {
                    if screen_size.is_some() {
                        diff_detector.reset();
                    }
                    screen_size = Some((width, height));
                    last_full_refresh = Instant::now();
                    Some((width, height))
                } else {
                    None
                };

                let force_full_refresh = last_full_refresh.elapsed() >= full_refresh_interval;
                if force_full_refresh {
                    diff_detector.invalidate_cache();
                    last_full_refresh = Instant::now();
                }

                let (changed_tiles, _) = diff_detector.detect_changes(&frame);

                if changed_tiles.is_empty() {
                    if let Some(size) = send_header {
                        if encode_tx.blocking_send((Some(size), vec![], vec![], vec![], Instant::now(), 0.0)).is_err() {
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

                let mut tiles_with_data: Vec<(Tile, usize, f32)> = merged_tiles
                    .iter()
                    .map(|tile| {
                        let tile_idx = (tile.y / tile_height * config.tiles_x + tile.x / tile_width) as usize;
                        let metadata = diff_detector.get_metadata(tile_idx);
                        let priority = Self::calculate_priority_static(tile, metadata, width, height, &config);
                        (*tile, tile_idx, priority)
                    })
                    .collect();

                tiles_with_data.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

                let sorted_tiles: Vec<Tile> = tiles_with_data.iter().map(|(t, _, _)| *t).collect();
                let sorted_tile_indices: Vec<usize> = tiles_with_data.iter().map(|(_, idx, _)| *idx).collect();

                let tile_hashes: Vec<u64> = sorted_tile_indices.iter()
                    .map(|&idx| diff_detector.get_current_hashes()[idx])
                    .collect();

                let mut encoded = vec![Vec::new(); sorted_tiles.len()];
                let mut submitted_count = 0;

                for (i, tile) in sorted_tiles.iter().enumerate() {
                    let tile_idx = sorted_tile_indices[i];

                    let metadata = diff_detector.get_metadata(tile_idx);
                    if metadata.cached_hash == tile_hashes[i] {
                        if let Some(ref cached) = metadata.cached_encoded {
                            encoded[i] = cached.clone();
                            continue;
                        }
                    }

                    let mut tile_buffer = tile_buffer_pool.get();
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

                    let task = crate::encoding_pool::EncodingTask {
                        tile: *tile,
                        tile_data: tile_buffer,
                        tile_idx,
                    };

                    let _ = encoding_pool.submit(task);
                    submitted_count += 1;
                }

                let encoded_results = encoding_pool.collect_results(submitted_count);

                let tile_idx_to_pos: HashMap<usize, usize> = sorted_tile_indices
                    .iter()
                    .enumerate()
                    .map(|(pos, &idx)| (idx, pos))
                    .collect();

                for result in encoded_results {
                    if let Some(&pos) = tile_idx_to_pos.get(&result.tile_idx) {
                        encoded[pos] = result.data;
                    } else {
                        eprintln!("⚠️  WARNING: Received result for unknown tile_idx={}", result.tile_idx);
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
                    eprintln!("❌ ERROR: Tile {} was not encoded (worker panic or channel overflow)", tile_idx);

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
                        ).unwrap_or_else(|_| Vec::new())
                    });

                    encoded[i] = fallback_encoded;
                }

                let tile_metadata = diff_detector.get_all_metadata_mut();
                for (i, &tile_idx) in sorted_tile_indices.iter().enumerate() {
                    let metadata = &mut tile_metadata[tile_idx];
                    if !encoded[i].is_empty() {
                        metadata.cached_encoded = Some(encoded[i].clone());
                        metadata.cached_hash = tile_hashes[i];
                    }
                }

                let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
                let timestamp = Instant::now();

                if encode_tx.blocking_send((send_header, sorted_tiles, encoded, sorted_tile_indices, timestamp, elapsed_ms)).is_err() {
                    break;
                }

                Self::sleep_until(deadline);
            }
        });

        // --- Async send loop with ACK tracking ---
        let mut frame_seq: u32 = 0;
        // seq → (sent_at, tile_indices)
        let mut pending_acks: HashMap<u32, (Instant, Vec<usize>)> = HashMap::new();
        let ack_timeout = Duration::from_millis(150);

        let mut frame_count = 0u64;
        let mut avg_ms = 0.0f64;

        while let Some((header, tiles, encoded, tile_indices, timestamp, elapsed_ms)) = encode_rx.recv().await {
            // Drain incoming ACKs
            while let Ok(acked_seq) = ack_rx.try_recv() {
                pending_acks.remove(&acked_seq);
            }

            // Find timed-out frames and push their tiles to stale list
            let now = Instant::now();
            let timed_out: Vec<u32> = pending_acks.iter()
                .filter(|(_, (sent_at, _))| now.duration_since(*sent_at) > ack_timeout)
                .map(|(seq, _)| *seq)
                .collect();

            if !timed_out.is_empty() {
                let mut stale = stale_tiles.lock().unwrap();
                for seq in timed_out {
                    if let Some((_, indices)) = pending_acks.remove(&seq) {
                        stale.extend(indices);
                    }
                }
            }

            if let Some((width, height)) = header {
                self.send_header_async(&data_channel, width, height).await?;
            }

            if !tiles.is_empty() {
                // Send sequence control packet: 0xFFFE (u16) | seq (u32) = 6 bytes
                frame_seq = frame_seq.wrapping_add(1);
                let seq = frame_seq;
                let mut seq_pkt = [0u8; 6];
                seq_pkt[0..2].copy_from_slice(&0xFFFEu16.to_le_bytes());
                seq_pkt[2..6].copy_from_slice(&seq.to_le_bytes());
                data_channel.send(&Bytes::copy_from_slice(&seq_pkt)).await?;

                pending_acks.insert(seq, (Instant::now(), tile_indices));

                let queue_time = timestamp.elapsed();
                let send_start = Instant::now();
                self.send_tiles_async(&data_channel, &tiles, &encoded).await?;
                let send_time = send_start.elapsed();

                let stats = FrameStats::new(tiles.len(), &encoded);
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

        let join_timeout = Duration::from_secs(5);
        match tokio::time::timeout(
            join_timeout,
            tokio::task::spawn_blocking(move || process_handle.join())
        ).await {
            Ok(Ok(Ok(_))) => println!("Processing thread finished successfully"),
            Ok(Ok(Err(e))) => {
                return Err(crate::error::Error::WebRTC(format!("Processing thread panicked: {:?}", e)));
            }
            Ok(Err(e)) => eprintln!("Failed to spawn blocking task: {:?}", e),
            Err(_) => eprintln!("⚠️  Processing thread did not finish within {}s", join_timeout.as_secs()),
        }

        Ok(())
    }

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
        if width > 65535 || height > 65535 {
            return Err(crate::error::Error::WebRTC(format!(
                "Screen resolution {}×{} exceeds protocol limit",
                width, height
            )));
        }

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
        let mut frame_data = {
            let mut buf = self.frame_data_buf.lock().unwrap();
            std::mem::take(&mut *buf)
        };
        let mut tile_buffer = {
            let mut buf = self.tile_buffer_buf.lock().unwrap();
            std::mem::take(&mut *buf)
        };

        let result = Self::do_send(data_channel, tiles, encoded, &mut frame_data, &mut tile_buffer).await;

        frame_data.clear();
        tile_buffer.clear();
        *self.frame_data_buf.lock().unwrap() = frame_data;
        *self.tile_buffer_buf.lock().unwrap() = tile_buffer;

        result
    }

    async fn do_send(
        data_channel: &Arc<RTCDataChannel>,
        tiles: &[Tile],
        encoded: &[Vec<u8>],
        frame_data: &mut Vec<u8>,
        tile_buffer: &mut Vec<u8>,
    ) -> Result<()> {
        const MAX_PACKET_SIZE: usize = 8_000;

        for (tile, webp) in tiles.iter().zip(encoded.iter()) {
            tile_buffer.clear();
            tile_buffer.extend_from_slice(&(tile.x as u16).to_le_bytes());
            tile_buffer.extend_from_slice(&(tile.y as u16).to_le_bytes());
            tile_buffer.extend_from_slice(&(tile.width as u16).to_le_bytes());
            tile_buffer.extend_from_slice(&(tile.height as u16).to_le_bytes());
            tile_buffer.extend_from_slice(webp);

            let tile_size = 4 + tile_buffer.len();

            if tile_size > MAX_PACKET_SIZE {
                if !frame_data.is_empty() {
                    data_channel.send(&Bytes::copy_from_slice(frame_data)).await?;
                    frame_data.clear();
                }
                let mut large_tile = Vec::with_capacity(tile_size);
                large_tile.extend_from_slice(&(tile_buffer.len() as u32).to_le_bytes());
                large_tile.extend_from_slice(tile_buffer);
                data_channel.send(&Bytes::from(large_tile)).await?;
                continue;
            }

            if !frame_data.is_empty() && frame_data.len() + tile_size > MAX_PACKET_SIZE {
                data_channel.send(&Bytes::copy_from_slice(frame_data)).await?;
                frame_data.clear();
            }

            frame_data.extend_from_slice(&(tile_buffer.len() as u32).to_le_bytes());
            frame_data.extend_from_slice(tile_buffer);
        }

        if !frame_data.is_empty() {
            data_channel.send(&Bytes::copy_from_slice(frame_data)).await?;
            frame_data.clear();
        }

        Ok(())
    }
}

struct FrameStats {
    tiles_sent: usize,
    total_kbits: f64,
}

impl FrameStats {
    fn new(tiles_sent: usize, encoded: &[Vec<u8>]) -> Self {
        let total_kbits = encoded.iter().map(|e| e.len()).sum::<usize>() as f64 * 8.0 / 1000.0;
        Self { tiles_sent, total_kbits }
    }
}
