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

/// Loads a TLS acceptor from the cert/key PEM file paths in `Config`
/// (`RING2ZERO_TLS_CERT` / `RING2ZERO_TLS_KEY`, e.g. from `tailscale cert`).
/// Safari requires a secure context for WebRTC, so remote/Safari clients
/// need `wss://` — this is what makes that possible.
///
/// Returns `Ok(None)` when neither path is set (TLS simply not requested).
/// Returns `Err` when TLS was requested but the cert/key couldn't be loaded —
/// the caller should fail startup on this rather than silently falling back
/// to plaintext, since a cert that's rotated out from under a running
/// deployment (or a typo'd path) should be loud, not a silent downgrade to
/// `ws://`.
fn load_tls_acceptor(
    cert_path: Option<&str>,
    key_path: Option<&str>,
) -> std::result::Result<Option<TlsAcceptor>, String> {
    let Some(cert_path) = cert_path else { return Ok(None) };
    let key_path = key_path
        .ok_or_else(|| "RING2ZERO_TLS_CERT is set but RING2ZERO_TLS_KEY is not".to_string())?;

    let cert_file = std::fs::File::open(cert_path)
        .map_err(|e| format!("Cannot open RING2ZERO_TLS_CERT ({cert_path}): {e}"))?;
    let mut cert_reader = std::io::BufReader::new(cert_file);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to parse RING2ZERO_TLS_CERT: {e}"))?;

    let key_file = std::fs::File::open(key_path)
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

/// Block-letter startup banner (bigmono12 figlet-style — thicker strokes
/// than the original mono12), printed once before anything else — only on
/// an actual terminal (never into a redirected log/systemd journal, where
/// ANSI art is just noise) and without color escapes when NO_COLOR is set
/// (https://no-color.org).
fn print_banner() {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return;
    }

    const LINES: [&str; 12] = [
        " ██████▒    ██████   ███   ██    ▒████▒            ░▓████▒   ████████  ████████  ██████▒    ░████░",
        " ███████▓   ██████   ███   ██   ▓██████            ███████▒  ████████  ████████  ███████▓   ██████",
        " ██   ▒██     ██     ███▒  ██  ▒██▒  ░█            █▒░  ▓██      ░███  ██        ██   ▒██  ▒██  ██▒",
        " ██    ██     ██     ████  ██  ██▒                       ██      ███   ██        ██    ██  ██▒  ▒██",
        " ██   ▒██     ██     ██▒█▒ ██  ██░                      ▒█▓     ▒██▒   ██        ██   ▒██  ██    ██",
        " ███████▒     ██     ██ ██ ██  ██                       ██      ███    ███████   ███████▒  ██    ██",
        " ██████▓      ██     ██ ██ ██  ██  ████               ░██▒     ███     ███████   ██████▓   ██    ██",
        " ██  ▓██░     ██     ██ ▒█▒██  ██░ ████   █████      ░██▒     ▒██▒     ██        ██  ▓██░  ██    ██",
        " ██   ██▓     ██     ██  ████  ██▒   ██   █████     ▒██▒      ██▓      ██        ██   ██▓  ██▒  ▒██",
        " ██   ▒██     ██     ██  ▒███  ▒██▒  ██            ▒██▒      ███       ██        ██   ▒██  ▒██  ██▒",
        " ██    ██▒  ██████   ██   ███   ███████            ████████  ████████  ████████  ██    ██▒  ██████",
        " ██    ███  ██████   ██   ███    ▒████░            ████████  ████████  ████████  ██    ███  ░████░",
    ];
    // #af3a03, true color.
    const COLOR: &str = "\x1b[38;2;175;58;3m";

    let no_color = std::env::var_os("NO_COLOR").is_some();

    println!();
    for line in LINES {
        if no_color {
            println!("{line}");
        } else {
            println!("{COLOR}{line}\x1b[0m");
        }
    }
    let subtitle = format!("  Wayland screen streamer over WebRTC · v{}", env!("CARGO_PKG_VERSION"));
    if no_color {
        println!("{subtitle}");
    } else {
        println!("{COLOR}{subtitle}\x1b[0m");
    }
    println!();
}

fn print_help() {
    println!(
        "ring-2zero — Wayland screen streaming server over WebRTC\n\
         \n\
         USAGE:\n\
         \x20   ring-2zero [OPTIONS]\n\
         \x20   r2zr [OPTIONS]           (shorthand alias — set up by install.sh)\n\
         \n\
         OPTIONS:\n\
         \x20   -h, --help       Print this help and exit\n\
         \x20   --no-adaptive    Skip the startup CPU benchmark, use merge_gap=0\n\
         \x20   --debug          Verbose per-tile/per-frame stats every 100 frames\n\
         \n\
         Once running, open http://<this-machine>:9001 in a browser — the auth\n\
         token printed on startup is the connection password. No separate client\n\
         file or static file server needed, the page is served by this binary.\n\
         \n\
         ENVIRONMENT VARIABLES:\n\
         \x20   RING2ZERO_TOKEN           Fixed auth token (default: random, printed on startup)\n\
         \x20   RING2ZERO_TLS_CERT/_KEY   PEM cert/key paths to serve wss:// (required for Safari/iOS)\n\
         \x20   RING2ZERO_ICE_INTERFACE   Restrict ICE candidate gathering to one interface\n\
         \x20   RING2ZERO_IPV4_ONLY       Set to exclude IPv6 ICE candidates\n\
         \x20   RING2ZERO_MAX_FPS         Cap target/static/dynamic FPS uniformly (1-1000)\n\
         \n\
         Don't have the r2zr alias yet? Run ./install.sh (or --no-alias to skip\n\
         everything else it does and just add the alias by hand — see its\n\
         source for the per-shell alias/abbr syntax).\n\
         \n\
         See README.md / docs/DEVELOPMENT.md for the full reference."
    );
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return Ok(());
    }

    print_banner();

    // Set RUST_LOG=ice=debug,webrtc_ice=debug,mdns=debug,webrtc_mdns=debug for
    // verbose ICE/mDNS connectivity diagnostics (candidate gathering, STUN
    // checks, mDNS query results) when troubleshooting a connection.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

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

    let tls_acceptor = match load_tls_acceptor(config.tls_cert_path.as_deref(), config.tls_key_path.as_deref()) {
        Ok(acceptor) => acceptor,
        Err(e) => {
            eprintln!("TLS setup failed: {e}");
            std::process::exit(1);
        }
    };
    let ws_scheme = if tls_acceptor.is_some() { "wss" } else { "ws" };
    let http_scheme = if tls_acceptor.is_some() { "https" } else { "http" };

    println!("WebRTC signaling server (WebSocket): {ws_scheme}://{addr}");
    if tls_acceptor.is_none() {
        println!("TLS disabled — set RING2ZERO_TLS_CERT/RING2ZERO_TLS_KEY for wss:// (required for Safari/iOS remote access)");
    }
    println!("Auth token: {}", config.auth_token);
    println!(
        "Open {http_scheme}://<this-host>:{} in a browser (password prompt uses the token above) — \
        no separate client file needed, this binary serves the page itself",
        config.ws_port
    );
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
