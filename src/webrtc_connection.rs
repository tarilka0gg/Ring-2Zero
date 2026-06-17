// WebRTC Connection Management Module
// Handles PeerConnection creation, configuration, and DataChannel setup

use crate::error::{Result, Error};
use crate::config::Config;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::ice_transport::ice_connection_state::RTCIceConnectionState;
use webrtc::ice_transport::ice_candidate::RTCIceCandidate;
use webrtc::data_channel::RTCDataChannel;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct WebRTCConnection {
    pub peer_connection: Arc<RTCPeerConnection>,
    pub data_channel: Arc<RTCDataChannel>,
}

pub struct IceChannel {
    pub ice_rx: mpsc::UnboundedReceiver<RTCIceCandidate>,
}

impl WebRTCConnection {
    /// Create a new WebRTC connection with low-latency settings
    /// Returns (connection, ice_channel)
    pub async fn new() -> Result<(Self, IceChannel)> {
        let m = MediaEngine::default();
        let mut s = webrtc::api::setting_engine::SettingEngine::default();

        // Aggressive timeouts for minimal latency
        s.set_ice_timeouts(
            Some(std::time::Duration::from_secs(5)),      // disconnected timeout
            Some(std::time::Duration::from_secs(10)),     // failed timeout
            Some(std::time::Duration::from_millis(500)),  // keepalive interval
        );

        let api = APIBuilder::new()
            .with_media_engine(m)
            .with_setting_engine(s)
            .build();

        // No STUN for local connection (minimal latency)
        let rtc_config = RTCConfiguration {
            ice_servers: vec![],
            ..Default::default()
        };

        let peer_connection = Arc::new(api.new_peer_connection(rtc_config).await?);

        // State change logging
        peer_connection.on_ice_connection_state_change(Box::new(move |state: RTCIceConnectionState| {
            println!("ICE state: {:?}", state);
            Box::pin(async {})
        }));

        peer_connection.on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
            println!("Peer state: {:?}", state);
            Box::pin(async {})
        }));

        // ICE candidate channel
        let (ice_tx, ice_rx) = mpsc::unbounded_channel::<RTCIceCandidate>();

        peer_connection.on_ice_candidate(Box::new(move |candidate: Option<RTCIceCandidate>| {
            let ice_tx = ice_tx.clone();
            Box::pin(async move {
                if let Some(candidate) = candidate {
                    let _ = ice_tx.send(candidate);
                }
            })
        }));

        // Create DataChannel with low-latency settings
        let dc_init = RTCDataChannelInit {
            ordered: Some(false),       // Disable ordering for lower latency
            max_retransmits: Some(0),   // No retransmits - better to skip frame
            ..Default::default()
        };

        let data_channel = peer_connection
            .create_data_channel("screen", Some(dc_init))
            .await?;

        Ok((
            Self {
                peer_connection,
                data_channel,
            },
            IceChannel { ice_rx },
        ))
    }

    /// Create and send offer, wait for ICE gathering
    pub async fn create_offer(&self) -> Result<String> {
        println!("Creating offer...");

        let offer = self.peer_connection.create_offer(None).await?;
        self.peer_connection.set_local_description(offer.clone()).await?;

        // Wait for ICE gathering
        let (ice_tx, mut ice_rx) = mpsc::channel::<()>(1);
        self.peer_connection.on_ice_gathering_state_change(Box::new(move |state| {
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

        let final_offer = self.peer_connection.local_description().await
            .ok_or_else(|| Error::WebRTC("No local description available".into()))?;

        Ok(final_offer.sdp)
    }

    /// Wait for DataChannel to open with timeout
    pub async fn wait_data_channel_open(&self, timeout_secs: u64) -> Result<bool> {
        let (tx, mut rx) = mpsc::channel::<()>(1);

        self.data_channel.on_open(Box::new(move || {
            let _ = tx.try_send(());
            Box::pin(async {})
        }));

        tokio::select! {
            _ = rx.recv() => {
                println!("DataChannel opened!");
                Ok(true)
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(timeout_secs)) => {
                eprintln!("DataChannel open timeout");
                Ok(false)
            }
        }
    }
}
