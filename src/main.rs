use screen_streamer::{
    capture::ScreenCapture,
    config::Config,
    error::{Result, Error},
    stream::StreamServer,
};

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
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::ice_transport::ice_connection_state::RTCIceConnectionState;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // Auto-detect optimal config based on CPU performance
    let mut config = if args.contains(&"--no-adaptive".to_string()) {
        println!("⚠️  Adaptive mode disabled, using defaults");
        Config::default()
    } else {
        Config::with_auto_merge_gap()
    };

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
            .map_err(|e| Error::WebRTC(format!("WebSocket upgrade failed: {}", e)))?;

        // Канал для відправки повідомлень через WebSocket
        let (ws_tx, mut ws_rx) = tokio::sync::mpsc::channel::<Message>(32);

        // Задача для обробки WebSocket
        let (mut ws_sender, mut ws_receiver) = ws_stream.split();

        tokio::spawn(async move {
            while let Some(msg) = ws_rx.recv().await {
                if let Err(e) = ws_sender.send(msg).await {
                    eprintln!("WebSocket send error: {}", e);
                    break;
                }
            }
        });

        // Створюємо WebRTC API
        let m = MediaEngine::default();
        let mut s = webrtc::api::setting_engine::SettingEngine::default();

        // Агресивні таймаути для мінімальної латентності
        s.set_ice_timeouts(
            Some(std::time::Duration::from_secs(5)),   // disconnected timeout
            Some(std::time::Duration::from_secs(10)),  // failed timeout
            Some(std::time::Duration::from_millis(500)), // keepalive interval
        );

        let api = APIBuilder::new()
            .with_media_engine(m)
            .with_setting_engine(s)
            .build();

        // Без STUN для локального з'єднання (мінімальна латентність)
        let rtc_config = RTCConfiguration {
            ice_servers: vec![],
            ..Default::default()
        };

        let peer_connection = Arc::new(api.new_peer_connection(rtc_config).await?);

        // Логування станів
        peer_connection.on_ice_connection_state_change(Box::new(move |state: RTCIceConnectionState| {
            println!("ICE state: {:?}", state);
            Box::pin(async {})
        }));

        peer_connection.on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
            println!("Peer state: {:?}", state);
            Box::pin(async {})
        }));

        // Канал для відправки ICE candidates
        let (ice_tx, mut ice_rx) = tokio::sync::mpsc::unbounded_channel::<webrtc::ice_transport::ice_candidate::RTCIceCandidate>();

        peer_connection.on_ice_candidate(Box::new(move |candidate: Option<webrtc::ice_transport::ice_candidate::RTCIceCandidate>| {
            let ice_tx = ice_tx.clone();
            Box::pin(async move {
                if let Some(candidate) = candidate {
                    let _ = ice_tx.send(candidate);
                }
            })
        }));

        // Задача для відправки ICE candidates через WebSocket
        let ws_tx_clone = ws_tx.clone();
        tokio::spawn(async move {
            while let Some(candidate) = ice_rx.recv().await {
                let candidate_json = serde_json::json!({
                    "type": "candidate",
                    "candidate": candidate.to_json().unwrap()
                });
                if ws_tx_clone.send(Message::Text(candidate_json.to_string())).await.is_err() {
                    eprintln!("Failed to send ICE candidate (channel full or closed)");
                    break;
                }
                println!("Sent ICE candidate to client");
            }
        });

        // Створюємо DataChannel з налаштуваннями для низької латентності
        let dc_init = RTCDataChannelInit {
            ordered: Some(false),        // Вимкнути упорядкування для зниження латентності
            max_retransmits: Some(0),    // Без ретрансмісій - краще пропустити кадр
            ..Default::default()
        };

        let data_channel = peer_connection
            .create_data_channel("screen", Some(dc_init))
            .await?;

        println!("Creating offer...");

        // Створюємо offer
        let offer = peer_connection.create_offer(None).await?;
        peer_connection.set_local_description(offer.clone()).await?;

        // Чекаємо ICE gathering
        let (ice_tx, mut ice_rx) = tokio::sync::mpsc::channel::<()>(1);
        peer_connection.on_ice_gathering_state_change(Box::new(move |state| {
            if state == webrtc::ice_transport::ice_gatherer_state::RTCIceGathererState::Complete {
                let _ = ice_tx.try_send(());
            }
            Box::pin(async {})
        }));

        tokio::select! {
            _ = ice_rx.recv() => {
                println!("ICE gathering complete");
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(10)) => {
                println!("ICE gathering timeout");
            }
        }

        let final_offer = peer_connection.local_description().await.unwrap();

        // Відправляємо offer
        let offer_json = serde_json::json!({
            "type": "offer",
            "sdp": final_offer.sdp
        });
        if ws_tx.send(Message::Text(offer_json.to_string())).await.is_err() {
            return Err(Error::WebRTC("Failed to send offer (channel full or closed)".into()));
        }

        println!("Offer sent, waiting for answer...");

        // Чекаємо answer (збільшений таймаут для мобільних)
        let mut answer_received = false;
        let timeout = tokio::time::Duration::from_secs(30);
        let deadline = tokio::time::Instant::now() + timeout;

        while !answer_received && tokio::time::Instant::now() < deadline {
            tokio::select! {
                msg_result = ws_receiver.next() => {
                    match msg_result {
                        Some(Ok(Message::Text(text))) => {
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                                if json.get("type").and_then(|v| v.as_str()) == Some("answer") {
                                    if let Some(sdp) = json.get("sdp").and_then(|v| v.as_str()) {
                                        println!("Got answer");

                                        let answer = RTCSessionDescription::answer(sdp.to_owned())?;
                                        peer_connection.set_remote_description(answer).await?;

                                        answer_received = true;

                                        // Чекаємо відкриття DataChannel
                                        let dc = Arc::clone(&data_channel);
                                        let config_clone = config.clone();

                                        let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);

                                        data_channel.on_open(Box::new(move || {
                                            let _ = tx.try_send(());
                                            Box::pin(async {})
                                        }));

                                        // Чекаємо відкриття
                                        tokio::select! {
                                            _ = rx.recv() => {
                                                println!("DataChannel opened!");

                                                // Запускаємо capture
                                                let (frame_tx, frame_rx) = mpsc::sync_channel(1);
                                                let stop = Arc::new(AtomicBool::new(false));
                                                let stop_capture = Arc::clone(&stop);

                                                let frame_duration = config_clone.frame_duration();
                                                let capture_thread = std::thread::spawn(move || {
                                                    let capture = ScreenCapture::new(frame_tx, stop_capture);
                                                    if let Err(e) = capture.run(frame_duration) {
                                                        eprintln!("Capture error: {e}");
                                                    }
                                                });

                                                // Запускаємо streaming
                                                let server = StreamServer::new(config_clone);
                                                if let Err(e) = server.handle_client_async(dc, frame_rx).await {
                                                    eprintln!("Streaming error: {e}");
                                                }

                                                stop.store(true, Ordering::Relaxed);
                                                let _ = capture_thread.join();
                                            }
                                            _ = tokio::time::sleep(tokio::time::Duration::from_secs(30)) => {
                                                eprintln!("DataChannel open timeout");
                                            }
                                        }

                                        break;
                                    }
                                } else if json.get("type").and_then(|v| v.as_str()) == Some("candidate") {
                                    // Обробляємо ICE candidates від клієнта
                                    if let Some(candidate_obj) = json.get("candidate") {
                                        if let Ok(candidate_init) = serde_json::from_value::<webrtc::ice_transport::ice_candidate::RTCIceCandidateInit>(candidate_obj.clone()) {
                                            if let Err(e) = peer_connection.add_ice_candidate(candidate_init).await {
                                                eprintln!("Failed to add ICE candidate: {}", e);
                                            } else {
                                                println!("Added ICE candidate from client");
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Some(Ok(Message::Close(_))) => {
                            println!("WebSocket closed");
                            break;
                        }
                        Some(Err(e)) => {
                            eprintln!("WebSocket error: {}", e);
                            break;
                        }
                        None => {
                            break;
                        }
                        _ => {}
                    }
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => {
                    continue;
                }
            }
        }

        if !answer_received {
            eprintln!("Answer timeout");
        }

    } else {
        // HTTP request - serve index.html
        println!("HTTP request");

        let html = tokio::fs::read_to_string("index.html").await?;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-cache\r\n\r\n{}",
            html.len(),
            html
        );
        stream.write_all(response.as_bytes()).await?;
    }

    Ok(())
}
