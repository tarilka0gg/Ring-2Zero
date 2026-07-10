// WebRTC Connection Management Module
// Handles PeerConnection creation, configuration, and DataChannel setup

use crate::config::Config;
use crate::error::{Result, Error};
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
use tokio::sync::{mpsc, Mutex};

pub struct WebRTCConnection {
    pub peer_connection: Arc<RTCPeerConnection>,
    pub data_channel: Arc<RTCDataChannel>,
    data_channel_open_rx: Mutex<mpsc::Receiver<()>>,
}

pub struct IceChannel {
    pub ice_rx: mpsc::UnboundedReceiver<RTCIceCandidate>,
}

impl WebRTCConnection {
    /// Create a new WebRTC connection with low-latency settings
    /// Returns (connection, ice_channel)
    pub async fn new(config: &Config) -> Result<(Self, IceChannel)> {
        let m = MediaEngine::default();
        let mut s = webrtc::api::setting_engine::SettingEngine::default();

        // Safari (and Chrome) obfuscate host ICE candidates behind a random
        // <uuid>.local mDNS name instead of the real LAN IP, as a privacy
        // measure. Without this, our ICE agent has no way to resolve that
        // name to an address, so every candidate from such a browser is
        // silently unusable and ICE can never find a working pair.
        s.set_ice_multicast_dns_mode(webrtc_ice::mdns::MulticastDnsMode::QueryOnly);

        // IPv4 only, opt-in: on a dual-stack host (e.g. a Tailscale interface
        // also offering an IPv6 ULA address alongside its IPv4 one), ICE can
        // gather multiple host candidates for the *same* logical path, and
        // without STUN priority tie-breaking the wrong one can get selected
        // first — if that path is actually flaky, the connection gets stuck
        // instead of falling back cleanly. This must stay opt-in rather than
        // unconditional: a deployment reachable only over IPv6 (some
        // CGNAT/VPN topologies) would otherwise gather zero usable
        // candidates and fail outright. Set RING2ZERO_IPV4_ONLY=1 to enable.
        if config.ice_ipv4_only {
            s.set_ip_filter(Box::new(|ip: std::net::IpAddr| ip.is_ipv4()));
        }

        // Optionally restrict ICE host-candidate gathering to a single named
        // interface (e.g. RING2ZERO_ICE_INTERFACE=tailscale0), so a machine
        // with multiple interfaces (LAN + VPN) doesn't advertise a LAN
        // candidate the remote peer can't actually reach.
        if let Some(iface) = config.ice_interface.clone() {
            s.set_interface_filter(Box::new(move |name: &str| name == iface));
        }

        // These were tuned aggressively (5s/10s/500ms) for same-machine,
        // near-zero-latency testing. Over a real network path (Wi-Fi,
        // mobile data, through a VPN like Tailscale) that's tight enough for
        // ordinary jitter to trip a "disconnected" state within a few missed
        // keepalives, silently killing the session after only the first
        // couple of messages went through. These values are closer to
        // typical browser defaults, tolerant of real-world latency/jitter.
        s.set_ice_timeouts(
            Some(std::time::Duration::from_secs(15)),     // disconnected timeout
            Some(std::time::Duration::from_secs(30)),     // failed timeout
            Some(std::time::Duration::from_secs(2)),      // keepalive interval
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
                    println!("ICE candidate: {} {}:{} typ={:?}", candidate.protocol, candidate.address, candidate.port, candidate.typ);
                    if let Err(_) = ice_tx.send(candidate) {
                        eprintln!("⚠️  WARNING: Failed to send ICE candidate (receiver dropped)");
                    }
                }
            })
        }));

        // Log the selected ICE candidate pair whenever it changes, via the
        // ICE transport's own change event — same event-driven pattern as
        // the on_ice_connection_state_change/on_peer_connection_state_change
        // callbacks above, instead of a hand-rolled poll-and-diff loop.
        peer_connection
            .sctp()
            .transport()
            .ice_transport()
            .on_selected_candidate_pair_change(Box::new(|pair| {
                println!("Selected candidate pair: {pair}");
                Box::pin(async {})
            }));

        // Create DataChannel: unordered (avoid head-of-line blocking latency)
        // but reliable (SCTP retransmits lost fragments). A tile message is
        // split into several SCTP chunks; with max_retransmits(0), losing any
        // single chunk drops the *entire* message with no recovery — over a
        // real lossy path (e.g. mobile data through a VPN) this made almost
        // all multi-chunk tile messages vanish while tiny single-chunk
        // control packets got through, since loss probability compounds per
        // chunk. Leaving retransmits enabled (None = reliable) fixes that;
        // the app-level ACK/stale-tile system still handles the remaining
        // rare loss/staleness cases.
        let dc_init = RTCDataChannelInit {
            ordered: Some(false),
            ..Default::default()
        };

        let data_channel = peer_connection
            .create_data_channel("screen", Some(dc_init))
            .await?;

        // Register on_open callback immediately to avoid race condition
        let (open_tx, open_rx) = mpsc::channel::<()>(1);
        data_channel.on_open(Box::new(move || {
            let _ = open_tx.try_send(());
            Box::pin(async {})
        }));

        Ok((
            Self {
                peer_connection,
                data_channel,
                data_channel_open_rx: Mutex::new(open_rx),
            },
            IceChannel { ice_rx },
        ))
    }

    /// Create and send offer, wait for ICE gathering
    pub async fn create_offer(&self) -> Result<String> {
        println!("Creating offer...");

        // Register BEFORE set_local_description to avoid missing Complete on fast LAN
        let (ice_tx, mut ice_rx) = mpsc::channel::<()>(1);
        self.peer_connection.on_ice_gathering_state_change(Box::new(move |state| {
            if state == webrtc::ice_transport::ice_gatherer_state::RTCIceGathererState::Complete {
                let _ = ice_tx.try_send(());
            }
            Box::pin(async {})
        }));

        let offer = self.peer_connection.create_offer(None).await?;
        self.peer_connection.set_local_description(offer.clone()).await?;

        // Wait for ICE gathering

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
        let mut rx = self.data_channel_open_rx.lock().await;

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

impl Drop for WebRTCConnection {
    /// `RTCPeerConnection` has no cleanup-on-drop of its own — its internal
    /// ICE agent, DTLS/SCTP transports and sockets are only released by an
    /// explicit `close()`. Without this, every reconnect (this server's
    /// whole reason for existing) would leak the previous session's
    /// connection object graph, and the candidate-pair monitor task above
    /// would never observe `RTCPeerConnectionState::Closed` and would spin
    /// forever. `close()` is async, so it's spawned as its own short-lived
    /// task rather than run here.
    fn drop(&mut self) {
        let pc = Arc::clone(&self.peer_connection);
        tokio::spawn(async move {
            if let Err(e) = pc.close().await {
                eprintln!("Failed to close peer connection: {e}");
            }
        });
    }
}
