mod capture;
mod config;
mod convert;
mod diff;
mod encoder;
mod error;
mod frame;
mod shm;
mod stream;
mod tile;

use capture::ScreenCapture;
use config::Config;
use error::Result;
use stream::StreamServer;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use futures_util::{SinkExt, StreamExt};
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::ice_transport::ice_gatherer_state::RTCIceGathererState;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;

#[tokio::main]
async fn main() -> Result<()> {
    let mut config = Config::default();

    // Перевіряємо чи є аргумент --debug
    let args: Vec<String> = std::env::args().collect();
    if args.contains(&"--debug".to_string()) {
        config.debug_mode = true;
        println!("[DEBUG MODE ENABLED]");
    }

    let addr = format!("0.0.0.0:{}", config.ws_port);
    let listener = TcpListener::bind(&addr).await?;

    println!("WebRTC signaling server (WebSocket): ws://{addr}");
    println!("Target FPS: {}", config.target_fps.get());
    println!("Dynamic tiles: {} FPS", config.dynamic_tile_fps.get());
    println!("Static tiles: {} FPS", config.static_tile_fps.get());

    loop {
        let (tcp_stream, _) = listener.accept().await?;
        let config = config.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_connection(tcp_stream, config).await {
                eprintln!("Помилка: {e}");
            }
        });
    }
}

async fn handle_connection(tcp_stream: tokio::net::TcpStream, config: Config) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Peek at the first bytes to determine if it's HTTP or WebSocket
    let mut buffer = [0u8; 1024];
    let mut stream = tcp_stream;

    // Read the request
    let n = stream.peek(&mut buffer).await?;
    let request = String::from_utf8_lossy(&buffer[..n]);

    // Check if it's a WebSocket upgrade request
    if request.contains("Upgrade: websocket") || request.contains("Upgrade: WebSocket") {
        println!("WebSocket upgrade request");

        // Upgrade to WebSocket
        let mut ws_stream = accept_async(stream).await
            .map_err(|e| crate::error::Error::WebRTC(format!("WebSocket upgrade failed: {}", e)))?;

    // Створюємо WebRTC API
    let m = MediaEngine::default();
    let api = APIBuilder::new()
        .with_media_engine(m)
        .build();

    // Конфігурація з публічним STUN сервером
    let rtc_config = RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
        }],
        ..Default::default()
    };

    let peer_connection = Arc::new(api.new_peer_connection(rtc_config).await?);

    // Створюємо DataChannel (сервер створює першим!)
    let dc_init = RTCDataChannelInit {
        ordered: Some(false),  // Unordered для мінімальної латентності
        ..Default::default()
    };

    let data_channel = peer_connection
        .create_data_channel("screen", Some(dc_init))
        .await?;

    println!("DataChannel created, generating offer...");

    // Створюємо offer
    let offer = peer_connection.create_offer(None).await?;
    peer_connection.set_local_description(offer.clone()).await?;

    // Чекаємо поки ICE gathering завершиться
    let (ice_tx, mut ice_rx) = tokio::sync::mpsc::channel::<()>(1);
    peer_connection.on_ice_gathering_state_change(Box::new(move |state| {
        if state == RTCIceGathererState::Complete {
            let _ = ice_tx.try_send(());
        }
        Box::pin(async {})
    }));

    // Чекаємо ICE gathering з timeout
    tokio::select! {
        _ = ice_rx.recv() => {
            println!("ICE gathering complete");
        }
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(3)) => {
            println!("ICE gathering timeout, proceeding anyway");
        }
    }

    // Отримуємо фінальний SDP з усіма ICE candidates
    let final_offer = peer_connection.local_description().await.unwrap();

    println!("Sending offer to client ({} bytes)", final_offer.sdp.len());

    // Відправляємо offer клієнту через WebSocket
    let offer_json = serde_json::json!({
        "type": "offer",
        "sdp": final_offer.sdp
    });
    ws_stream.send(Message::Text(offer_json.to_string())).await
        .map_err(|e| crate::error::Error::WebRTC(format!("Failed to send offer: {}", e)))?;

    println!("Waiting for answer (timeout: 5 seconds)...");

    // Даємо клієнту час на ICE gathering (до 5 секунд)
    let answer_timeout = tokio::time::Duration::from_secs(5);
    let answer_deadline = tokio::time::Instant::now() + answer_timeout;

    // Чекаємо answer від клієнта з timeout
    let answer_result = tokio::time::timeout_at(answer_deadline, ws_stream.next()).await;

    match answer_result {
        Ok(Some(Ok(msg))) => {
            if let Message::Text(text) = msg {
                println!("Received answer from client ({} bytes)", text.len());

            let json: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| crate::error::Error::WebRTC(format!("Invalid JSON: {}", e)))?;

            println!("Answer JSON: {:?}", json);

            if let Some(sdp) = json["sdp"].as_str() {
                let answer = RTCSessionDescription::answer(sdp.to_owned())?;
                peer_connection.set_remote_description(answer).await?;

                println!("Answer set, waiting for DataChannel to open...");

                let dc = Arc::clone(&data_channel);
                let config_clone = config.clone();

                // Чекаємо поки DataChannel відкриється
                let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);

                data_channel.on_open(Box::new(move || {
                    let _ = tx.try_send(());
                    Box::pin(async {})
                }));

                // Чекаємо відкриття з timeout
                tokio::select! {
                    _ = rx.recv() => {
                        println!("DataChannel opened!");

                        // Запускаємо capture thread
                        let (frame_tx, frame_rx) = mpsc::sync_channel(1);
                        let stop = Arc::new(AtomicBool::new(false));
                        let stop_capture = Arc::clone(&stop);

                        let frame_duration = config_clone.frame_duration();
                        let capture_thread = std::thread::spawn(move || {
                            let capture = ScreenCapture::new(frame_tx, stop_capture);
                            if let Err(e) = capture.run(frame_duration) {
                                eprintln!("Capture помилка: {e}");
                            }
                        });

                        // Запускаємо streaming
                        let server = StreamServer::new(config_clone);
                        if let Err(e) = server.handle_client_async(dc, frame_rx).await {
                            eprintln!("Streaming помилка: {e}");
                        }

                        stop.store(true, Ordering::Relaxed);
                        let _ = capture_thread.join();
                    }
                    _ = tokio::time::sleep(tokio::time::Duration::from_secs(10)) => {
                        eprintln!("DataChannel open timeout");
                    }
                }
            } else {
                eprintln!("Answer JSON missing sdp field");
            }
        } else {
            eprintln!("Received non-text message");
        }
        }
        Ok(Some(Err(e))) => {
            eprintln!("WebSocket error while waiting for answer: {}", e);
        }
        Ok(None) => {
            eprintln!("WebSocket closed before answer received");
        }
        Err(_) => {
            eprintln!("Timeout waiting for answer (5 seconds)");
        }
    }
    } else {
        // HTTP request - serve index.html
        println!("HTTP request for index.html");

        let html = tokio::fs::read_to_string("index.html").await?;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-cache, no-store, must-revalidate\r\nPragma: no-cache\r\nExpires: 0\r\n\r\n{}",
            html.len(),
            html
        );
        stream.write_all(response.as_bytes()).await?;
    }

    Ok(())
}
