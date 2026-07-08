// WebRTC Signaling Module
// Handles WebSocket-based signaling for WebRTC connection establishment

use crate::error::{Result, Error};
use tokio_tungstenite::tungstenite::Message;
use tokio::sync::mpsc;
use futures_util::StreamExt;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::ice_transport::ice_candidate::RTCIceCandidate;
use std::sync::Arc;

pub struct SignalingChannel {
    ws_tx: mpsc::Sender<Message>,
    ice_rx: mpsc::UnboundedReceiver<RTCIceCandidate>,
}

impl SignalingChannel {
    pub fn new(
        ws_tx: mpsc::Sender<Message>,
        ice_rx: mpsc::UnboundedReceiver<RTCIceCandidate>,
    ) -> Self {
        Self { ws_tx, ice_rx }
    }

    /// Send offer through WebSocket, then start forwarding ICE candidates.
    /// `session` tags this negotiation attempt so late-arriving answer/candidate
    /// messages from an abandoned attempt can be told apart from the current one.
    pub async fn send_offer_and_start_forwarding(mut self, sdp: String, session: u64) -> Result<()> {
        let offer_json = serde_json::json!({
            "type": "offer",
            "sdp": sdp,
            "session": session
        });
        self.ws_tx.send(Message::Text(offer_json.to_string())).await
            .map_err(|_| Error::WebRTC("Failed to send offer (channel full or closed)".into()))?;

        // Start ICE forwarding in background
        tokio::spawn(async move {
            while let Some(candidate) = self.ice_rx.recv().await {
                let candidate_str = match candidate.to_json() {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("Failed to serialize ICE candidate, skipping: {}", e);
                        continue;
                    }
                };
                let candidate_json = serde_json::json!({
                    "type": "candidate",
                    "candidate": candidate_str,
                    "session": session
                });
                if self.ws_tx.send(Message::Text(candidate_json.to_string())).await.is_err() {
                    eprintln!("Failed to send ICE candidate (channel full or closed)");
                    break;
                }
                println!("Sent ICE candidate to client");
            }
        });

        Ok(())
    }
}

/// Wait for answer from client via WebSocket.
/// `session` is the current negotiation attempt's id (see
/// `SignalingChannel::send_offer_and_start_forwarding`); answer/candidate
/// messages tagged with a different (stale) session are ignored, since the
/// same `ws_receiver` is reused across reconnect attempts and a late message
/// from an abandoned attempt must not be applied to this `peer_connection`.
pub async fn wait_for_answer(
    ws_receiver: &mut futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>>,
    peer_connection: Arc<RTCPeerConnection>,
    timeout_secs: u64,
    session: u64,
) -> Result<bool> {
    let timeout = tokio::time::Duration::from_secs(timeout_secs);
    let deadline = tokio::time::Instant::now() + timeout;

    while tokio::time::Instant::now() < deadline {
        tokio::select! {
            msg_result = ws_receiver.next() => {
                match msg_result {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                            let msg_session = json.get("session").and_then(|v| v.as_u64());
                            if msg_session != Some(session) {
                                println!("Ignoring stale signaling message from session {:?} (current: {session})", msg_session);
                                continue;
                            }
                            if json.get("type").and_then(|v| v.as_str()) == Some("answer") {
                                if let Some(sdp) = json.get("sdp").and_then(|v| v.as_str()) {
                                    println!("Got answer");
                                    let answer = RTCSessionDescription::answer(sdp.to_owned())?;
                                    peer_connection.set_remote_description(answer).await?;
                                    return Ok(true);
                                }
                            } else if json.get("type").and_then(|v| v.as_str()) == Some("candidate") {
                                // Handle ICE candidates from client
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
                        return Ok(false);
                    }
                    Some(Err(e)) => {
                        eprintln!("WebSocket error: {}", e);
                        return Ok(false);
                    }
                    None => return Ok(false),
                    _ => {}
                }
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => {
                continue;
            }
        }
    }

    Ok(false)
}
