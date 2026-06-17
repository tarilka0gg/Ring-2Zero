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
use tokio::io::AsyncWriteExt;

/// Handle incoming TCP connection - dispatch to WebSocket or HTTP handler
pub async fn handle_connection(tcp_stream: TcpStream, config: Config) -> Result<()> {
    let mut buffer = [0u8; 1024];
    let mut stream = tcp_stream;

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

/// Handle WebSocket connection and establish WebRTC
async fn handle_websocket_connection(stream: TcpStream, config: Config) -> Result<()> {
    let ws_stream = accept_async(stream).await
        .map_err(|e| Error::WebRTC(format!("WebSocket upgrade failed: {}", e)))?;

    // Channel for sending messages through WebSocket
    let (ws_tx, mut ws_rx) = tokio::sync::mpsc::channel::<Message>(32);

    // Split WebSocket stream
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    // Task for sending WebSocket messages
    tokio::spawn(async move {
        while let Some(msg) = ws_rx.recv().await {
            if let Err(e) = ws_sender.send(msg).await {
                eprintln!("WebSocket send error: {}", e);
                break;
            }
        }
    });

    // Create WebRTC connection
    let (webrtc_conn, ice_channel) = WebRTCConnection::new().await?;

    // Setup signaling channel
    let signaling = SignalingChannel::new(ws_tx.clone(), ice_channel.ice_rx);
    signaling.start_ice_forwarding();

    // Create and send offer
    let offer_sdp = webrtc_conn.create_offer().await?;
    let (_dummy_tx, dummy_rx) = tokio::sync::mpsc::unbounded_channel();
    let signal_tx = SignalingChannel::new(ws_tx.clone(), dummy_rx);
    signal_tx.send_offer(offer_sdp).await?;

    println!("Offer sent, waiting for answer...");

    // Wait for answer from client
    let answer_received = wait_for_answer(
        &mut ws_receiver,
        Arc::clone(&webrtc_conn.peer_connection),
        30,
    ).await?;

    if !answer_received {
        return Err(Error::WebRTC("Failed to receive answer within timeout".into()));
    }

    // Wait for DataChannel to open
    if !webrtc_conn.wait_data_channel_open(30).await? {
        return Err(Error::WebRTC("DataChannel failed to open".into()));
    }

    // Start screen capture
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

    // Start streaming
    let server = StreamServer::new(config);
    if let Err(e) = server.handle_client_async(webrtc_conn.data_channel, frame_rx).await {
        eprintln!("Streaming error: {e}");
    }

    // Cleanup
    stop.store(true, Ordering::Relaxed);
    let _ = capture_thread.join();

    Ok(())
}
