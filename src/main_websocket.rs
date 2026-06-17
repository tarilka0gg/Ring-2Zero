mod capture;
mod config;
mod convert;
mod diff;
mod encoder;
mod error;
mod frame;
mod shm;
mod tile;

use capture::ScreenCapture;
use config::Config;
use diff::DiffDetector;
use encoder::{TileEncoder, TileMerger};
use error::Result;
use frame::Frame;
use tile::Tile;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use futures_util::{SinkExt, StreamExt};

#[tokio::main]
async fn main() -> Result<()> {
    let mut config = Config::default();

    let args: Vec<String> = std::env::args().collect();
    if args.contains(&"--debug".to_string()) {
        config.debug_mode = true;
        println!("[DEBUG MODE ENABLED]");
    }

    let addr = format!("0.0.0.0:{}", config.ws_port);
    let listener = TcpListener::bind(&addr).await?;

    println!("WebSocket server: ws://{addr}");
    println!("Target FPS: {}", config.target_fps.get());
    println!("Dynamic tiles: {} FPS", config.dynamic_tile_fps.get());
    println!("Static tiles: {} FPS", config.static_tile_fps.get());

    loop {
        let (tcp_stream, _) = listener.accept().await?;
        let config = config.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_connection(tcp_stream, config).await {
                eprintln!("Connection error: {e}");
            }
        });
    }
}

async fn handle_connection(tcp_stream: tokio::net::TcpStream, config: Config) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    let mut buffer = [0u8; 1024];
    let mut stream = tcp_stream;

    let n = stream.peek(&mut buffer).await?;
    let request = String::from_utf8_lossy(&buffer[..n]);

    if request.contains("Upgrade: websocket") || request.contains("Upgrade: WebSocket") {
        println!("WebSocket connection");

        let ws_stream = accept_async(stream).await
            .map_err(|e| crate::error::Error::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("WebSocket upgrade failed: {}", e)
            )))?;

        let (ws_sender, _ws_receiver) = ws_stream.split();

        // Запускаємо capture thread
        let (frame_tx, frame_rx) = mpsc::sync_channel(0); // Rendezvous channel
        let stop = Arc::new(AtomicBool::new(false));
        let stop_capture = Arc::clone(&stop);

        let frame_duration = config.frame_duration();
        let capture_thread = std::thread::spawn(move || {
            let capture = ScreenCapture::new(frame_tx, stop_capture);
            if let Err(e) = capture.run(frame_duration) {
                eprintln!("Capture error: {e}");
            }
        });

        // Запускаємо streaming
        if let Err(e) = handle_client_websocket(ws_sender, frame_rx, config).await {
            eprintln!("Streaming error: {e}");
        }

        stop.store(true, Ordering::Relaxed);
        let _ = capture_thread.join();

    } else {
        // HTTP request - serve index.html
        println!("HTTP request");

        let html = tokio::fs::read_to_string("index-websocket.html").await?;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-cache\r\n\r\n{}",
            html.len(),
            html
        );
        stream.write_all(response.as_bytes()).await?;
    }

    Ok(())
}

async fn handle_client_websocket(
    mut ws_sender: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        Message
    >,
    frame_rx: mpsc::Receiver<Frame>,
    config: Config,
) -> Result<()> {
    println!("Client connected!");

    let tile_encoder = TileEncoder::new(config.clone());
    let mut diff_detector = DiffDetector::new(config.clone());
    let tile_merger = TileMerger::new(config.merge_gap);
    let mut screen_size: Option<(u32, u32)> = None;

    let frame_duration = config.frame_duration();
    let mut frame_count = 0u64;
    let mut avg_ms = 0.0f64;

    loop {
        let deadline = Instant::now() + frame_duration;

        let frame = match receive_frame(&frame_rx, deadline) {
            Some(f) => f,
            None => continue,
        };

        let (width, height) = (frame.width, frame.height);
        let t0 = Instant::now();

        // Відправляємо header якщо розмір змінився
        if screen_size.map_or(true, |s| s != (width, height)) {
            diff_detector.reset();
            screen_size = Some((width, height));
            send_header(&mut ws_sender, width, height).await?;
        }

        let (changed_tiles, tile_indices) = diff_detector.detect_changes(&frame);

        if changed_tiles.is_empty() {
            sleep_until(deadline);
            continue;
        }

        let tile_width = width / config.tiles_x;
        let tile_height = tile_width * height / width;
        let tiles_y = (height + tile_height - 1) / tile_height;

        let merged_tiles = tile_merger.merge(
            &changed_tiles,
            config.tiles_x,
            tiles_y,
            tile_width,
            tile_height,
            width,
            height,
        );

        let mut tiles_with_priority: Vec<(Tile, usize, f32)> = merged_tiles
            .iter()
            .enumerate()
            .map(|(idx, tile)| {
                let tile_idx = tile_indices[idx];
                let metadata = diff_detector.get_metadata(tile_idx);
                let priority = calculate_priority(tile, metadata, width, height, &config);
                (*tile, idx, priority)
            })
            .collect();

        tiles_with_priority.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

        let sorted_tiles: Vec<Tile> = tiles_with_priority.iter().map(|(t, _, _)| *t).collect();

        // Encoding
        let encoded = tile_encoder.encode_tiles(&sorted_tiles, &frame.rgba, width);

        // Відправляємо тайли
        send_tiles(&mut ws_sender, &sorted_tiles, &encoded).await?;

        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        frame_count += 1;
        avg_ms += (elapsed_ms - avg_ms) / frame_count as f64;

        let total_kbits = encoded.iter().map(|e| e.len()).sum::<usize>() as f64 * 8.0 / 1000.0;

        println!(
            "{} тайлів / {:.1} кбіт / {:.1} мс / сер. {:.1} мс",
            sorted_tiles.len(), total_kbits, elapsed_ms, avg_ms
        );

        sleep_until(deadline);
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
                eprintln!("Capture thread disconnected");
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

fn calculate_priority(tile: &Tile, metadata: &crate::tile::TileMetadata, width: u32, height: u32, config: &Config) -> f32 {
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

async fn send_header(
    ws_sender: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        Message
    >,
    width: u32,
    height: u32,
) -> Result<()> {
    let mut header = Vec::with_capacity(6);
    header.extend_from_slice(&0xFFFFu16.to_le_bytes());
    header.extend_from_slice(&(width as u16).to_le_bytes());
    header.extend_from_slice(&(height as u16).to_le_bytes());
    ws_sender.send(Message::Binary(header)).await
        .map_err(|e| crate::error::Error::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Send header failed: {}", e)
        )))?;
    Ok(())
}

async fn send_tiles(
    ws_sender: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        Message
    >,
    tiles: &[Tile],
    encoded: &[Vec<u8>],
) -> Result<()> {
    // Менші пакети для нижчої латентності
    const MAX_PACKET_SIZE: usize = 8_000;

    let mut frame_data = Vec::with_capacity(MAX_PACKET_SIZE);
    let mut tile_buffer = Vec::with_capacity(1 << 16);

    for (tile, webp) in tiles.iter().zip(encoded.iter()) {
        tile_buffer.clear();
        tile_buffer.extend_from_slice(&(tile.x as u16).to_le_bytes());
        tile_buffer.extend_from_slice(&(tile.y as u16).to_le_bytes());
        tile_buffer.extend_from_slice(&(tile.width as u16).to_le_bytes());
        tile_buffer.extend_from_slice(&(tile.height as u16).to_le_bytes());
        tile_buffer.extend_from_slice(webp);

        let tile_size = 4 + tile_buffer.len();

        // Відправляємо пакет якщо він заповнений
        if !frame_data.is_empty() && frame_data.len() + tile_size > MAX_PACKET_SIZE {
            ws_sender.send(Message::Binary(frame_data.clone())).await
                .map_err(|e| crate::error::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Send tiles failed: {}", e)
                )))?;
            frame_data.clear();
        }

        frame_data.extend_from_slice(&(tile_buffer.len() as u32).to_le_bytes());
        frame_data.extend_from_slice(&tile_buffer);
    }

    // Відправляємо залишок
    if !frame_data.is_empty() {
        ws_sender.send(Message::Binary(frame_data)).await
            .map_err(|e| crate::error::Error::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Send tiles failed: {}", e)
            )))?;
    }

    Ok(())
}
