use screen_streamer::{
    config::Config,
    error::Result,
    server::{handle_connection, handle_connection_tls},
};

use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::ServerConfig as RustlsServerConfig;
use tokio_rustls::TlsAcceptor;

/// Loads a TLS acceptor from RING2ZERO_TLS_CERT / RING2ZERO_TLS_KEY (PEM files),
/// e.g. from `tailscale cert`. Safari requires a secure context for WebRTC, so
/// remote/Safari clients need `wss://` — this is what makes that possible.
///
/// Returns `Ok(None)` when neither env var is set (TLS simply not requested).
/// Returns `Err` when TLS was requested but the cert/key couldn't be loaded —
/// the caller should fail startup on this rather than silently falling back
/// to plaintext, since a cert that's rotated out from under a running
/// deployment (or a typo'd path) should be loud, not a silent downgrade to
/// `ws://`.
fn load_tls_acceptor() -> std::result::Result<Option<TlsAcceptor>, String> {
    let cert_path = match std::env::var("RING2ZERO_TLS_CERT") {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    let key_path = std::env::var("RING2ZERO_TLS_KEY")
        .map_err(|_| "RING2ZERO_TLS_CERT is set but RING2ZERO_TLS_KEY is not".to_string())?;

    let cert_file = std::fs::File::open(&cert_path)
        .map_err(|e| format!("Cannot open RING2ZERO_TLS_CERT ({cert_path}): {e}"))?;
    let mut cert_reader = std::io::BufReader::new(cert_file);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to parse RING2ZERO_TLS_CERT: {e}"))?;

    let key_file = std::fs::File::open(&key_path)
        .map_err(|e| format!("Cannot open RING2ZERO_TLS_KEY ({key_path}): {e}"))?;
    let mut key_reader = std::io::BufReader::new(key_file);
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key_reader)
        .map_err(|e| format!("Failed to parse RING2ZERO_TLS_KEY: {e}"))?
        .ok_or_else(|| "RING2ZERO_TLS_KEY contains no private key".to_string())?;

    let tls_config = RustlsServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("Invalid TLS certificate/key pair: {e}"))?;

    Ok(Some(TlsAcceptor::from(Arc::new(tls_config))))
}

#[tokio::main]
async fn main() -> Result<()> {
    // Set RUST_LOG=ice=debug,webrtc_ice=debug,mdns=debug,webrtc_mdns=debug for
    // verbose ICE/mDNS connectivity diagnostics (candidate gathering, STUN
    // checks, mDNS query results) when troubleshooting a connection.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

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

    // Quick bandwidth-constrained testing knob: RING2ZERO_MAX_FPS=5 caps
    // target/static/dynamic tile FPS uniformly, without touching the
    // adaptive-quality machinery. Clamped to 1000: Config::frame_duration()
    // is `1000 / fps` in whole milliseconds, so anything above 1000 would
    // truncate to a 0ms duration — turning the cap into an uncapped
    // busy-loop instead of throttling anything.
    if let Ok(fps_str) = std::env::var("RING2ZERO_MAX_FPS") {
        if let Ok(fps) = fps_str.parse::<u64>() {
            let fps = fps.clamp(1, 1000);
            let nz = std::num::NonZeroU64::new(fps).unwrap();
            config.target_fps = nz;
            config.static_tile_fps = nz;
            config.dynamic_tile_fps = nz;
            println!("[RING2ZERO_MAX_FPS] capped all FPS knobs to {fps}");
        }
    }

    let addr = format!("0.0.0.0:{}", config.ws_port);
    let listener = TcpListener::bind(&addr).await?;

    let tls_acceptor = match load_tls_acceptor() {
        Ok(acceptor) => acceptor,
        Err(e) => {
            eprintln!("TLS setup failed: {e}");
            std::process::exit(1);
        }
    };
    let scheme = if tls_acceptor.is_some() { "wss" } else { "ws" };

    println!("WebRTC signaling server (WebSocket): {scheme}://{addr}");
    if tls_acceptor.is_none() {
        println!("TLS disabled — set RING2ZERO_TLS_CERT/RING2ZERO_TLS_KEY for wss:// (required for Safari/iOS remote access)");
    }
    println!("Auth token: {}", config.auth_token);
    println!("Connect clients with: client.html?server=<host>:{} (password prompt uses the token above)", config.ws_port);
    println!("Target FPS: {}", config.target_fps.get());
    println!("Dynamic tiles: {} FPS", config.dynamic_tile_fps.get());
    println!("Static tiles: {} FPS", config.static_tile_fps.get());

    loop {
        let (tcp_stream, _) = listener.accept().await?;
        let config = config.clone();

        if let Some(acceptor) = tls_acceptor.clone() {
            tokio::spawn(async move {
                match acceptor.accept(tcp_stream).await {
                    Ok(tls_stream) => {
                        if let Err(e) = handle_connection_tls(tls_stream, config).await {
                            eprintln!("Connection error: {e}");
                        }
                    }
                    Err(e) => eprintln!("TLS handshake failed: {e}"),
                }
            });
        } else {
            tokio::spawn(async move {
                if let Err(e) = handle_connection(tcp_stream, config).await {
                    eprintln!("Connection error: {e}");
                }
            });
        }
    }
}
