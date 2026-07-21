// HTTP/WebSocket Server Module
// Handles incoming connections and dispatches to WebRTC/WebSocket handlers

use crate::error::{Result, Error};
use crate::config::Config;
use crate::webrtc_connection::WebRTCConnection;
use crate::signaling::{SignalingChannel, wait_for_answer};
use crate::capture::ScreenCapture;
use crate::stream::StreamServer;

use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::task::{Context as TaskContext, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::http::StatusCode;
use futures_util::{SinkExt, StreamExt};

/// The browser client, baked into the binary so `ring-2zero` is a single
/// self-contained executable — no separate static file server, no path to
/// remember, and it's served from whatever host/port the WebSocket signaling
/// itself is reachable on (including over Tailscale/TLS), so the page's own
/// `location.host` is always the right WebSocket address to default to.
const CLIENT_HTML: &str = include_str!("../docs/client-examples/client.html");

/// Handle incoming plaintext TCP connection - dispatch to WebSocket or HTTP handler
pub async fn handle_connection(tcp_stream: TcpStream, config: Config) -> Result<()> {
    let mut buffer = [0u8; 1024];
    let stream = tcp_stream;

    // peek (not read) so the WebSocket upgrade path below still sees these
    // bytes at the start of the stream — accept_hdr_async needs to parse the
    // full HTTP upgrade request itself.
    let n = stream.peek(&mut buffer).await?;
    let request = String::from_utf8_lossy(&buffer[..n]);

    if request.contains("Upgrade: websocket") || request.contains("Upgrade: WebSocket") {
        println!("WebSocket connection");
        handle_websocket_connection(stream, config).await
    } else if request.starts_with("GET ") {
        serve_client_html(stream).await
    } else {
        Err(Error::WebRTC("Unrecognized connection (not a WebSocket upgrade or HTTP GET)".into()))
    }
}

/// Handle an already-TLS-terminated connection (see `main.rs`'s TLS acceptor).
/// Safari requires a secure context for WebRTC, so remote/Safari clients need
/// `wss://` here rather than `ws://` — and since the client page is now
/// served from this same port, that secure context covers the page load too.
pub async fn handle_connection_tls<S>(mut stream: S, config: Config) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // Unlike a raw TcpStream there's no kernel-level peek on a generic TLS
    // stream, so the sniffed bytes have to be read (consumed) and then
    // handed back via PrefixedStream before the WebSocket upgrade parses the
    // request itself.
    let mut buffer = [0u8; 1024];
    let n = stream.read(&mut buffer).await?;
    let request = String::from_utf8_lossy(&buffer[..n]).into_owned();

    if request.contains("Upgrade: websocket") || request.contains("Upgrade: WebSocket") {
        println!("WebSocket connection (TLS)");
        let wrapped = PrefixedStream::new(buffer[..n].to_vec(), stream);
        handle_websocket_connection(wrapped, config).await
    } else if request.starts_with("GET ") {
        serve_client_html(stream).await
    } else {
        Err(Error::WebRTC("Unrecognized connection (not a WebSocket upgrade or HTTP GET)".into()))
    }
}

/// Serves the embedded client page over plain HTTP GET. Single-page tool, so
/// every path resolves to the same response — there's nothing else to route.
async fn serve_client_html<S: AsyncWrite + Unpin>(mut stream: S) -> Result<()> {
    let body = CLIENT_HTML.as_bytes();
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await?;
    Ok(())
}

/// Replays a prefix of already-consumed bytes before continuing to read from
/// the wrapped stream — lets a stream get "un-consumed" after sniffing its
/// first bytes on a transport (TLS) that has no native peek.
struct PrefixedStream<S> {
    prefix: Vec<u8>,
    prefix_pos: usize,
    inner: S,
}

impl<S> PrefixedStream<S> {
    fn new(prefix: Vec<u8>, inner: S) -> Self {
        Self { prefix, prefix_pos: 0, inner }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for PrefixedStream<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        if self.prefix_pos < self.prefix.len() {
            let remaining = &self.prefix[self.prefix_pos..];
            let n = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..n]);
            self.prefix_pos += n;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for PrefixedStream<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Extract a query parameter's value from a URI's query string (e.g. `token=abc` in `?token=abc&x=1`).
fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| {
        let mut parts = kv.splitn(2, '=');
        if parts.next()? == key { parts.next() } else { None }
    })
}

/// Handle WebSocket connection and establish WebRTC — with auto-reconnect
async fn handle_websocket_connection<S>(stream: S, config: Config) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let expected_token = config.auth_token.clone();
    let ws_stream = accept_hdr_async(stream, move |req: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
        let provided = req.uri().query().and_then(|q| query_param(q, "token"));
        if provided == Some(expected_token.as_str()) {
            Ok(response)
        } else {
            let resp = tokio_tungstenite::tungstenite::http::Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body(Some("Unauthorized: missing or invalid token".to_string()))
                .unwrap();
            Err(resp)
        }
    }).await
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

    let mut session: u64 = 0;

    loop {
        // Check if the WebSocket sender task is still running before trying to reconnect
        if ws_tx.is_closed() {
            println!("WebSocket closed, stopping");
            break;
        }

        session += 1;

        let (webrtc_conn, ice_channel) = match WebRTCConnection::new(&config).await {
            Ok(x) => x,
            Err(e) => { eprintln!("WebRTC init failed: {e}"); break; }
        };

        let offer_sdp = match webrtc_conn.create_offer().await {
            Ok(s) => s,
            Err(e) => { eprintln!("create_offer failed: {e}"); break; }
        };

        let signaling = SignalingChannel::new(ws_tx.clone(), ice_channel.ice_rx);
        if signaling.send_offer_and_start_forwarding(offer_sdp, session).await.is_err() {
            eprintln!("Failed to send offer — WebSocket likely closed");
            break;
        }

        println!("Offer sent, waiting for answer...");

        let answer_received = wait_for_answer(
            &mut ws_receiver,
            Arc::clone(&webrtc_conn.peer_connection),
            30,
            session,
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
            let capture = match ScreenCapture::new(frame_tx, stop_capture) {
                Ok(c) => c,
                Err(e) => { eprintln!("Capture setup failed: {e}"); return; }
            };
            if let Err(e) = capture.run(frame_duration) {
                eprintln!("Capture error: {e}");
            }
        });

        let server = StreamServer::new(config.clone());
        match server.handle_client_async(Arc::clone(&webrtc_conn.data_channel), frame_rx).await {
            Ok(_) => println!("Stream ended normally, attempting reconnect..."),
            Err(e) => eprintln!("Stream error: {e}, attempting reconnect..."),
        }

        stop.store(true, Ordering::Relaxed);
        // Run the blocking join on a dedicated blocking-pool thread, with a
        // timeout, so a wedged capture backend can't stall this Tokio worker
        // (and the other connections' tasks scheduled onto it) indefinitely.
        let join_result = tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            tokio::task::spawn_blocking(move || capture_thread.join()),
        ).await;
        if join_result.is_err() {
            eprintln!("Capture thread did not stop within 5s, abandoning it");
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_param_finds_the_requested_key() {
        assert_eq!(query_param("token=abc&x=1", "token"), Some("abc"));
        assert_eq!(query_param("x=1&token=abc", "token"), Some("abc"));
    }

    #[test]
    fn query_param_returns_none_for_a_missing_key() {
        assert_eq!(query_param("x=1&y=2", "token"), None);
    }

    #[test]
    fn query_param_returns_none_for_a_valueless_key() {
        // `token` with no `=value` at all shouldn't be confused with a
        // present-but-empty value.
        assert_eq!(query_param("token&x=1", "token"), None);
    }

    #[test]
    fn query_param_handles_an_empty_value() {
        assert_eq!(query_param("token=&x=1", "token"), Some(""));
    }

    #[tokio::test]
    async fn prefixed_stream_replays_the_prefix_before_the_inner_stream() {
        let (mut tx, rx) = tokio::io::duplex(64);
        tx.write_all(b"world").await.unwrap();
        drop(tx); // EOF after "world" so the final read terminates

        let mut wrapped = PrefixedStream::new(b"hello ".to_vec(), rx);
        let mut out = Vec::new();
        wrapped.read_to_end(&mut out).await.unwrap();
        assert_eq!(out, b"hello world");
    }

    #[tokio::test]
    async fn prefixed_stream_writes_pass_through_to_the_inner_stream() {
        let (tx, mut rx) = tokio::io::duplex(64);
        let mut wrapped = PrefixedStream::new(Vec::new(), tx);
        wrapped.write_all(b"ping").await.unwrap();

        let mut buf = [0u8; 4];
        rx.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }

    #[tokio::test]
    async fn serve_client_html_writes_a_200_response_with_the_full_page() {
        let (tx, mut rx) = tokio::io::duplex(CLIENT_HTML.len() + 4096);
        serve_client_html(tx).await.unwrap();

        let mut out = Vec::new();
        rx.read_to_end(&mut out).await.unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains(&format!("Content-Length: {}", CLIENT_HTML.as_bytes().len())));
        assert!(text.ends_with(CLIENT_HTML));
    }
}
