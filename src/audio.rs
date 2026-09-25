//! Audio streaming: `src_c/pw_audio_capture.c` records the default sink's
//! monitor ("what you hear") directly from PipeWire on a dedicated OS
//! thread, this module frames the raw interleaved f32 samples into Opus's
//! fixed 20ms packets and encodes them, and [`AudioSession`] writes each
//! packet to the caller's audio track.
//!
//! Only built with `--features audio_capture` (pulls in libopus + a second
//! libpipewire link, on top of `pipewire_capture`'s own). Gated behind
//! `Config::audio`, same as `clipboard`/`input` are gated behind
//! `Config::control` — the operator opts in explicitly, since this is their
//! desktop audio going out over the network.
//!
//! Same safety posture as `capture::pipewire`: buffer bounds are enforced
//! on the C side (see pw_audio_capture.c's on_process), and — like every
//! other Wayland/PipeWire-facing change in this session — this was not
//! live-tested against a real compositor/audio session; see CHANGELOG.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use webrtc::media::Sample;

use crate::error::{Error, Result};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Opus's own fixed 20ms frame size at 48kHz — not a tunable, the encoder
/// only accepts a handful of specific frame lengths and this is the
/// standard one every WebRTC client already expects.
const FRAME_SAMPLES_PER_CHANNEL: usize = 960;
const FRAME_SAMPLES_TOTAL: usize = FRAME_SAMPLES_PER_CHANNEL * CHANNELS;
const FRAME_DURATION: Duration = Duration::from_millis(20);

extern "C" {
    fn pw_audio_capture_start(
        on_audio: unsafe extern "C" fn(*const u8, u32, u32, u32, *mut libc::c_void),
        user_data: *mut libc::c_void,
        stop_flag: *const libc::c_int,
        err_buf: *mut libc::c_char,
        err_len: libc::c_int,
    ) -> libc::c_int;
}

struct CallbackState {
    tx: tokio::sync::mpsc::Sender<Vec<f32>>,
}

unsafe extern "C" fn on_audio_cb(
    data: *const u8,
    n_samples: u32,
    channels: u32,
    rate: u32,
    ud: *mut libc::c_void,
) {
    // We only ever negotiate one exact format (see build_audio_format_pod
    // in the C side); anything else would mean the negotiation path
    // changed underneath us, not a value worth trying to resample here.
    if channels as usize != CHANNELS || rate != SAMPLE_RATE || n_samples == 0 {
        return;
    }
    let state = &*(ud as *const CallbackState);

    // `data` is a PipeWire-owned mmap'd buffer with no guaranteed alignment
    // for `f32`; reading it as a `&[f32]` directly would be a misaligned
    // read (UB). Going through `chunks_exact(4)` + `from_ne_bytes` copies
    // each sample out one at a time instead, which is alignment-agnostic.
    let byte_len = n_samples as usize * std::mem::size_of::<f32>();
    let raw = std::slice::from_raw_parts(data, byte_len);
    let samples: Vec<f32> = raw
        .chunks_exact(4)
        .map(|c| f32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    // A full mpsc channel means the encoder task has fallen behind; drop
    // this chunk rather than block PipeWire's real-time capture thread.
    let _ = state.tx.try_send(samples);
}

/// A running audio-capture-and-encode session; call
/// [`detach`](Self::detach) when the stream ends.
pub struct AudioSession {
    stop: Arc<AtomicBool>,
    capture_thread: Option<std::thread::JoinHandle<()>>,
    encode_task: tokio::task::JoinHandle<()>,
}

/// Starts capturing the host's audio output and encoding it as Opus,
/// writing each 20ms packet to `track`.
pub fn attach(
    track: Arc<webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample>,
) -> Result<AudioSession> {
    let mut encoder = opus::Encoder::new(
        SAMPLE_RATE,
        opus::Channels::Stereo,
        opus::Application::Audio,
    )
    .map_err(|e| Error::Audio(format!("opus encoder init: {e}")))?;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<f32>>(64);
    let stop = Arc::new(AtomicBool::new(false));

    let capture_thread = {
        let stop = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("audio-capture".into())
            .spawn(move || run_capture_thread(tx, stop))
            .map_err(|e| Error::Audio(format!("spawn audio capture thread: {e}")))?
    };

    let encode_task = tokio::spawn(async move {
        let mut pending: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES_TOTAL * 2);
        let mut out_buf = vec![0u8; 4000]; // generous upper bound for a 20ms Opus frame

        while let Some(chunk) = rx.recv().await {
            pending.extend_from_slice(&chunk);

            while pending.len() >= FRAME_SAMPLES_TOTAL {
                let frame: Vec<f32> = pending.drain(..FRAME_SAMPLES_TOTAL).collect();
                match encoder.encode_float(&frame, &mut out_buf) {
                    Ok(len) => {
                        let sample = Sample {
                            data: bytes::Bytes::copy_from_slice(&out_buf[..len]),
                            duration: FRAME_DURATION,
                            ..Default::default()
                        };
                        if track.write_sample(&sample).await.is_err() {
                            return; // peer connection's gone; outer teardown calls detach()
                        }
                    }
                    Err(e) => log::warn!("Opus encode failed: {e}"),
                }
            }
        }
    });

    Ok(AudioSession {
        stop,
        capture_thread: Some(capture_thread),
        encode_task,
    })
}

fn run_capture_thread(tx: tokio::sync::mpsc::Sender<Vec<f32>>, stop: Arc<AtomicBool>) {
    let state = Box::new(CallbackState { tx });
    let state_ptr = Box::into_raw(state);

    // Same C-int stop-flag bridge as capture::pipewire::PipeWireCapture::run.
    let stop_int = Arc::new(AtomicI32::new(0));
    let stop_ref = Arc::as_ptr(&stop_int) as *const libc::c_int;

    let watcher_stop = Arc::clone(&stop);
    let watcher_stop_int = Arc::clone(&stop_int);
    let watcher = std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(50));
        if watcher_stop.load(Ordering::Relaxed) {
            watcher_stop_int.store(1, Ordering::SeqCst);
            break;
        }
    });

    let mut err_buf = vec![0i8; 256];
    let ret = unsafe {
        pw_audio_capture_start(
            on_audio_cb,
            state_ptr as *mut libc::c_void,
            stop_ref,
            err_buf.as_mut_ptr(),
            err_buf.len() as libc::c_int,
        )
    };

    let _ = unsafe { Box::from_raw(state_ptr) };
    drop(watcher); // exits on its own via `stop`, same reasoning as the video capture path

    if ret < 0 {
        let msg = unsafe { std::ffi::CStr::from_ptr(err_buf.as_ptr()) }.to_string_lossy();
        log::error!("Audio capture failed: {msg}");
    }
}

impl AudioSession {
    /// Stops capturing and waits for the capture thread and encoder task to
    /// finish.
    pub async fn detach(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.capture_thread.take() {
            let _ = tokio::task::spawn_blocking(move || t.join()).await;
        }
        if tokio::time::timeout(Duration::from_secs(2), self.encode_task)
            .await
            .is_err()
        {
            log::warn!("Audio encode task did not stop within 2s");
        }
    }
}
