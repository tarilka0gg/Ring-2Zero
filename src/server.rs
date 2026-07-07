// HTTP/WebSocket Server Module
// Handles incoming connections and dispatches to WebRTC/WebSocket handlers

use crate::error::{Result, Error};
use crate::config::Config;
use crate::webrtc_connection::WebRTCConnection;
use crate::signaling::{SignalingChannel, wait_for_answer};
use crate::capture::ScreenCapture;
use crate::stream::StreamServer;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use tokio::net::TcpStream;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use futures_util::{SinkExt, StreamExt};

/// Handle incoming TCP connection - dispatch to WebSocket or HTTP handler
pub async fn handle_connection(tcp_stream: TcpStream, config: Config) -> Result<()> {
    let mut buffer = [0u8; 1024];
    let stream = tcp_stream;

    let n = stream.peek(&mut buffer).await?;
    let request = String::from_utf8_lossy(&buffer[..n]);

    if request.contains("Upgrade: websocket") || request.contains("Upgrade: WebSocket") {
        println!("WebSocket connection");
        handle_websocket_connection(stream, config).await
    } else {
        // Could add HTTP handler here for status page, etc.
        Err(Error::WebRTC("Non-WebSocket connections not supported".into()))
    }
}

/// Handle WebSocket connection and establish WebRTC — with auto-reconnect
async fn handle_websocket_connection(stream: TcpStream, config: Config) -> Result<()> {
    let ws_stream = accept_async(stream).await
        .map_err(|e| Error::WebRTC(format!("WebSocket upgrade failed: {}", e)))?;

    let (ws_tx, mut ws_rx) = tokio::sync::mpsc::channel::<Message>(32);
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    // Dedicated task for WebSocket sends — kept alive across reconnects
    tokio::spawn(async move {
        while let Some(msg) = ws_rx.recv().await {
            if let Err(e) = ws_sender.send(msg).await {
                eprintln!("WebSocket send error: {}", e);
                break;
            }
        }
    });

    loop {
        // Check if the WebSocket sender task is still running before trying to reconnect
        if ws_tx.is_closed() {
            println!("WebSocket closed, stopping");
            break;
        }

        let (webrtc_conn, ice_channel) = match WebRTCConnection::new().await {
            Ok(x) => x,
            Err(e) => { eprintln!("WebRTC init failed: {e}"); break; }
        };

        let offer_sdp = match webrtc_conn.create_offer().await {
            Ok(s) => s,
            Err(e) => { eprintln!("create_offer failed: {e}"); break; }
        };

        let signaling = SignalingChannel::new(ws_tx.clone(), ice_channel.ice_rx);
        if signaling.send_offer_and_start_forwarding(offer_sdp).await.is_err() {
            eprintln!("Failed to send offer — WebSocket likely closed");
            break;
        }

        println!("Offer sent, waiting for answer...");

        let answer_received = wait_for_answer(
            &mut ws_receiver,
            Arc::clone(&webrtc_conn.peer_connection),
            30,
        ).await.unwrap_or(false);

        if !answer_received {
            eprintln!("No answer received within timeout, closing");
            break;
        }

        if !webrtc_conn.wait_data_channel_open(30).await.unwrap_or(false) {
            eprintln!("DataChannel failed to open, closing");
            break;
        }

        let (frame_tx, frame_rx) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_capture = Arc::clone(&stop);
        let frame_duration = config.frame_duration();

        let capture_thread = std::thread::spawn(move || {
            let capture = ScreenCapture::new(frame_tx, stop_capture);
            if let Err(e) = capture.run(frame_duration) {
                eprintln!("Capture error: {e}");
            }
        });

        let server = StreamServer::new(config.clone());
        match server.handle_client_async(webrtc_conn.data_channel, frame_rx).await {
            Ok(_) => println!("Stream ended normally, attempting reconnect..."),
            Err(e) => eprintln!("Stream error: {e}, attempting reconnect..."),
        }

        stop.store(true, Ordering::Relaxed);
        let _ = capture_thread.join();

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    Ok(())
}
