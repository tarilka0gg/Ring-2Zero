//! Bidirectional clipboard sync over the `clipboard` DataChannel (only
//! present with `--control`, since it lets the browser read and overwrite
//! the host's clipboard).
//!
//! Uses `wl-clipboard-rs`, which speaks both `wlr-data-control` and
//! `ext-data-control` — works on wlroots compositors *and* GNOME/KDE, unlike
//! `input.rs`'s hand-rolled virtual-pointer/keyboard (those protocols really
//! are wlroots-only). It also does its own safe FD handling internally, so
//! this module never touches a raw pipe.
//!
//! There's no "clipboard changed" event to subscribe to (`wlr-data-control`
//! has one — `selection` — but this crate doesn't expose a long-lived
//! watch API over it), so the host side is polled instead. That's a
//! deliberate, acceptable trade for clipboard sync: nobody needs
//! sub-second propagation of a copy.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use wl_clipboard_rs::copy;
use wl_clipboard_rs::paste;

/// How often the host clipboard is polled for a change to push to the browser.
const POLL_INTERVAL: Duration = Duration::from_millis(700);

/// Sanity bound on either direction — not a real limit any legitimate
/// clipboard text would hit, just a guard against a malformed or hostile
/// message trying to push something absurd through.
const MAX_LEN: usize = 1 << 20; // 1 MiB

/// A running clipboard-sync session; call [`detach`](Self::detach) when the
/// stream ends.
pub struct ClipboardSession {
    channel: Arc<RTCDataChannel>,
    task: tokio::task::JoinHandle<()>,
}

/// Starts syncing the host clipboard with the `clipboard` DataChannel.
/// Each message on it, either direction, is the new clipboard text as raw
/// UTF-8 bytes — no framing needed, a DataChannel message is already one
/// discrete unit.
pub fn attach(channel: &Arc<RTCDataChannel>) -> ClipboardSession {
    let (from_browser_tx, from_browser_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    channel.on_message(Box::new(move |msg: DataChannelMessage| {
        if !msg.is_string && msg.data.len() <= MAX_LEN {
            if let Ok(text) = String::from_utf8(msg.data.to_vec()) {
                let _ = from_browser_tx.send(text);
            } else {
                log::debug!("Ignoring non-UTF8 clipboard message");
            }
        }
        Box::pin(async {})
    }));

    let dc = Arc::clone(channel);
    let task = tokio::spawn(run(dc, from_browser_rx));
    ClipboardSession {
        channel: Arc::clone(channel),
        task,
    }
}

async fn run(
    dc: Arc<RTCDataChannel>,
    mut from_browser: tokio::sync::mpsc::UnboundedReceiver<String>,
) {
    // The last text this session itself set on either side, so writing the
    // browser's paste back to the host doesn't immediately get read back on
    // the very next poll and re-sent to the browser as if the host had
    // changed it independently.
    let mut last_seen: Option<String> = None;
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            msg = from_browser.recv() => {
                let Some(text) = msg else { break }; // detach() replaced on_message, dropping the sender
                let for_task = text.clone();
                match tokio::task::spawn_blocking(move || set_host_clipboard(&for_task)).await {
                    Ok(Ok(())) => last_seen = Some(text),
                    Ok(Err(e)) => log::warn!("Failed to set host clipboard: {e}"),
                    Err(e) => log::error!("Clipboard set task panicked: {e}"),
                }
            }
            _ = ticker.tick() => {
                match tokio::task::spawn_blocking(read_host_clipboard).await {
                    Ok(Ok(Some(text))) if Some(&text) != last_seen.as_ref() => {
                        last_seen = Some(text.clone());
                        if dc.send(&Bytes::from(text.into_bytes())).await.is_err() {
                            break; // channel's gone; the outer session teardown will call detach()
                        }
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => log::debug!("Clipboard poll: {e}"),
                    Err(e) => log::error!("Clipboard poll task panicked: {e}"),
                }
            }
        }
    }
}

fn set_host_clipboard(text: &str) -> Result<(), copy::Error> {
    copy::Options::new().copy(
        copy::Source::Bytes(text.as_bytes().to_vec().into_boxed_slice()),
        copy::MimeType::Text,
    )
}

/// `Ok(None)` covers both "nothing on the clipboard" and "something's
/// there, but it isn't text" — neither is an error worth logging on every
/// single poll tick.
fn read_host_clipboard() -> Result<Option<String>, paste::Error> {
    use paste::{ClipboardType, Error, MimeType, Seat};
    let (reader, _mime) =
        match paste::get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Text) {
            Ok(ok) => ok,
            Err(Error::ClipboardEmpty | Error::NoMimeType | Error::NoSeats) => return Ok(None),
            Err(e) => return Err(e),
        };
    let mut buf = Vec::new();
    reader
        .take(MAX_LEN as u64)
        .read_to_end(&mut buf)
        .map_err(Error::SocketOpenError)?;
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

impl ClipboardSession {
    /// Stops syncing and waits for the current poll/set, if any, to finish.
    pub async fn detach(self) {
        // Same trick as input::InputSession::detach: replacing the handler
        // drops the old closure and with it the only sender, which ends
        // `run`'s receive loop at its next iteration.
        self.channel.on_message(Box::new(|_| Box::pin(async {})));
        if tokio::time::timeout(Duration::from_secs(2), self.task)
            .await
            .is_err()
        {
            log::warn!("Clipboard task did not stop within 2s");
        }
    }
}

// No unit tests: read_host_clipboard/set_host_clipboard need a real Wayland
// compositor with a data-control-capable seat, and the logic worth testing
// (dedup against last_seen, the recv-None shutdown path) lives entirely
// inside `run`'s tokio::select!, which would need a fake RTCDataChannel to
// exercise — not worth the harness for a ~30-line function. Covered instead
// by live verification, same as this session's other Wayland-facing changes
// (see input.rs).
