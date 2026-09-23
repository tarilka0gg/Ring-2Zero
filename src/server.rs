// HTTP/WebSocket Server Module
// Handles incoming connections and dispatches to WebRTC/WebSocket handlers

use crate::auth::{self, AuthOutcome};
use crate::capture::ScreenCapture;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::input;
use crate::signaling::{wait_for_answer, SignalingChannel};
use crate::stream;
use crate::webrtc_connection::WebRTCConnection;

use futures_util::{SinkExt, StreamExt};
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::task::{Context as TaskContext, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;

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
    // bytes at the start of the stream — accept_async needs to parse the
    // full HTTP upgrade request itself.
    let n = stream.peek(&mut buffer).await?;
    let request = String::from_utf8_lossy(&buffer[..n]);

    if is_websocket_upgrade(&request) {
        log::info!("WebSocket connection");
        handle_websocket_connection(stream, config).await
    } else if request.starts_with("GET ") {
        serve_client_html(stream).await
    } else {
        Err(Error::WebRTC(
            "Unrecognized connection (not a WebSocket upgrade or HTTP GET)".into(),
        ))
    }
}

/// Whether the sniffed request head is a WebSocket upgrade. Header names and
/// the `websocket` token are case-insensitive (RFC 9110 / RFC 6455): browsers
/// send `Upgrade: websocket`, but e.g. Node's WebSocket and proxies that
/// normalise headers send `upgrade: websocket`, which a case-sensitive check
/// misrouted to the HTML page.
fn is_websocket_upgrade(request: &str) -> bool {
    request
        .lines()
        .skip(1)
        .take_while(|l| !l.is_empty())
        .any(|line| {
            line.split_once(':').is_some_and(|(name, value)| {
                name.trim().eq_ignore_ascii_case("upgrade")
                    && value.trim().eq_ignore_ascii_case("websocket")
            })
        })
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

    if is_websocket_upgrade(&request) {
        log::info!("WebSocket connection (TLS)");
        let wrapped = PrefixedStream::new(buffer[..n].to_vec(), stream);
        handle_websocket_connection(wrapped, config).await
    } else if request.starts_with("GET ") {
        serve_client_html(stream).await
    } else {
        Err(Error::WebRTC(
            "Unrecognized connection (not a WebSocket upgrade or HTTP GET)".into(),
        ))
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
        Self {
            prefix,
            prefix_pos: 0,
            inner,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for PrefixedStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
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
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// First message after a successful auth: what the client needs to build a
/// matching `RTCPeerConnection` and UI.
fn hello_message(config: &Config) -> String {
    serde_json::json!({
        "type": "hello",
        "version": env!("CARGO_PKG_VERSION"),
        "ice_servers": config.ice_servers,
        "control": config.control,
    })
    .to_string()
}

/// Handle WebSocket connection and establish WebRTC — with auto-reconnect
async fn handle_websocket_connection<S>(stream: S, config: Config) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let ws_stream = accept_async(stream)
        .await
        .map_err(|e| Error::WebRTC(format!("WebSocket upgrade failed: {e}")))?;
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    match auth::authenticate(&mut ws_receiver, &config.auth_token).await {
        AuthOutcome::Accepted => {}
        AuthOutcome::Rejected => {
            log::warn!("Rejected a client with a wrong token");
            tokio::time::sleep(auth::FAILURE_DELAY).await;
            let close = CloseFrame {
                code: CloseCode::from(auth::CLOSE_UNAUTHORIZED),
                reason: "unauthorized".into(),
            };
            let _ = ws_sender.send(Message::Close(Some(close))).await;
            return Ok(());
        }
        AuthOutcome::Gone => return Ok(()),
    }
    ws_sender
        .send(Message::Text(hello_message(&config)))
        .await
        .map_err(|e| Error::WebRTC(format!("Failed to send hello: {e}")))?;

    let (ws_tx, mut ws_rx) = tokio::sync::mpsc::channel::<Message>(32);

    // Dedicated task for WebSocket sends — kept alive across reconnects
    tokio::spawn(async move {
        while let Some(msg) = ws_rx.recv().await {
            if let Err(e) = ws_sender.send(msg).await {
                log::error!("WebSocket send error: {}", e);
                break;
            }
        }
    });

    let mut session: u64 = 0;

    loop {
        // Check if the WebSocket sender task is still running before trying to reconnect
        if ws_tx.is_closed() {
            log::info!("WebSocket closed, stopping");
            break;
        }

        session += 1;

        let (webrtc_conn, ice_channel) = match WebRTCConnection::new(&config).await {
            Ok(x) => x,
            Err(e) => {
                log::error!("WebRTC init failed: {e}");
                break;
            }
        };

        let offer_sdp = match webrtc_conn.create_offer().await {
            Ok(s) => s,
            Err(e) => {
                log::error!("create_offer failed: {e}");
                break;
            }
        };

        let signaling = SignalingChannel::new(ws_tx.clone(), ice_channel.ice_rx);
        if signaling
            .send_offer_and_start_forwarding(offer_sdp, session)
            .await
            .is_err()
        {
            log::error!("Failed to send offer — WebSocket likely closed");
            break;
        }

        log::info!("Offer sent, waiting for answer...");

        let answer_received = wait_for_answer(
            &mut ws_receiver,
            Arc::clone(&webrtc_conn.peer_connection),
            30,
            session,
        )
        .await
        .unwrap_or(false);

        if !answer_received {
            log::warn!("No answer received within timeout, closing");
            break;
        }

        if !webrtc_conn
            .wait_data_channel_open(30)
            .await
            .unwrap_or(false)
        {
            log::error!("DataChannel failed to open, closing");
            break;
        }

        let (frame_tx, frame_rx) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_capture = Arc::clone(&stop);
        let frame_duration = config.frame_duration();

        let capture_thread = std::thread::spawn(move || {
            let capture = match ScreenCapture::new(frame_tx, stop_capture) {
                Ok(c) => c,
                Err(e) => {
                    log::error!("Capture setup failed: {e}");
                    return;
                }
            };
            match capture.run(frame_duration) {
                // The session ended and dropped its receiver: normal teardown.
                Ok(()) | Err(Error::ConsumerDisconnected) => {}
                Err(e) => log::error!("Capture error: {e}"),
            }
        });

        let input = webrtc_conn.input_channel.as_ref().map(input::attach);

        // Keep reading the WebSocket while streaming: otherwise a closed tab
        // or a client-initiated close goes unnoticed (the browser hangs in
        // CLOSING waiting for our close reply) and capture + encoding keep
        // running until SCTP times out ~30 s later.
        let mut session = Box::pin(stream::run_session(
            config.clone(),
            Arc::clone(&webrtc_conn.data_channel),
            frame_rx,
        ));
        let client_gone = loop {
            tokio::select! {
                result = &mut session => {
                    match result {
                        Ok(()) => log::info!("Stream ended, renegotiating"),
                        Err(e) => log::warn!("Stream error: {e}, renegotiating"),
                    }
                    break false;
                }
                msg = ws_receiver.next() => match msg {
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                        log::info!("Client closed the WebSocket, ending the session");
                        break true;
                    }
                    // Late messages of this or an older negotiation (e.g.
                    // trailing ICE candidates) — nothing to do mid-stream.
                    Some(Ok(_)) => {}
                },
            }
        };
        // Dropping an unfinished session closes its encode channel, which
        // stops the processing thread right away.
        drop(session);

        if let Some(session) = input {
            session.detach().await;
        }
        stop.store(true, Ordering::Relaxed);
        // Run the blocking join on a dedicated blocking-pool thread, with a
        // timeout, so a wedged capture backend can't stall this Tokio worker
        // (and the other connections' tasks scheduled onto it) indefinitely.
        let join_result = tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            tokio::task::spawn_blocking(move || capture_thread.join()),
        )
        .await;
        if join_result.is_err() {
            log::warn!("Capture thread did not stop within 5s, abandoning it");
        }

        if client_gone {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn websocket_upgrade_detection_is_case_insensitive() {
        assert!(is_websocket_upgrade(
            "GET / HTTP/1.1\r\nHost: h\r\nUpgrade: websocket\r\n\r\n"
        ));
        assert!(is_websocket_upgrade(
            "GET / HTTP/1.1\r\nupgrade: websocket\r\n\r\n"
        ));
        assert!(is_websocket_upgrade(
            "GET / HTTP/1.1\r\nUPGRADE:  WebSocket \r\n\r\n"
        ));
        assert!(!is_websocket_upgrade("GET / HTTP/1.1\r\nHost: h\r\n\r\n"));
        assert!(
            !is_websocket_upgrade("GET /Upgrade: websocket HTTP/1.1\r\n\r\n"),
            "request line isn't a header"
        );
        assert!(!is_websocket_upgrade(
            "GET / HTTP/1.1\r\nX-Note: upgrade: websocket\r\n\r\n"
        ));
    }

    #[test]
    fn hello_carries_ice_servers_and_control_flag() {
        let config = Config {
            ice_servers: crate::ice::parse_ice_servers("stun:s.example").unwrap(),
            control: true,
            ..Config::default()
        };
        let v: serde_json::Value = serde_json::from_str(&hello_message(&config)).unwrap();
        assert_eq!(v["type"], "hello");
        assert_eq!(v["control"], true);
        assert_eq!(v["ice_servers"][0]["urls"][0], "stun:s.example");
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
        assert!(text.contains(&format!("Content-Length: {}", CLIENT_HTML.len())));
        assert!(text.ends_with(CLIENT_HTML));
    }
}
