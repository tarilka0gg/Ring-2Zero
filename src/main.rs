use screen_streamer::{
    config::Config,
    error::Result,
    server::handle_connection,
};

use tokio::net::TcpListener;

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
