use crate::capture::CaptureBackend;
use crate::convert::convert_bgrx_to_rgba_inplace;
use crate::error::{Error, Result};
use crate::frame::Frame;

use std::sync::{atomic::{AtomicBool, AtomicI32, Ordering}, mpsc, Arc};
use std::time::Duration;

// SPA video format constants (from spa/param/video/raw.h)
const SPA_VIDEO_FORMAT_BGRX: u32 = 8;
const SPA_VIDEO_FORMAT_RGBA: u32 = 11;
const SPA_VIDEO_FORMAT_BGRA: u32 = 12;

extern "C" {
    fn pw_capture_start(
        on_frame:  unsafe extern "C" fn(*const u8, u32, u32, u32, u32, *mut libc::c_void),
        user_data: *mut libc::c_void,
        stop_flag: *const libc::c_int,
        err_buf:   *mut libc::c_char,
        err_len:   libc::c_int,
    ) -> libc::c_int;
}

// Shared state passed as user_data to the C callback
struct CallbackState {
    tx:   mpsc::SyncSender<Frame>,
    rgba: std::cell::UnsafeCell<Vec<u8>>,
}

unsafe impl Sync for CallbackState {}

unsafe extern "C" fn on_frame_cb(
    data:    *const u8,
    width:   u32,
    height:  u32,
    stride:  u32,
    spa_fmt: u32,
    ud:      *mut libc::c_void,
) {
    let state = &*(ud as *const CallbackState);
    let size  = (stride * height) as usize;
    let raw   = std::slice::from_raw_parts(data, size);
    let rgba  = &mut *state.rgba.get();

    match spa_fmt {
        SPA_VIDEO_FORMAT_BGRA | SPA_VIDEO_FORMAT_BGRX => {
            convert_bgrx_to_rgba_inplace(raw, width, height, rgba);
        }
        SPA_VIDEO_FORMAT_RGBA => {
            rgba.resize(size, 0);
            rgba.copy_from_slice(raw);
        }
        _ => {
            convert_bgrx_to_rgba_inplace(raw, width, height, rgba);
        }
    }

    let frame_data = std::mem::take(rgba);
    // Empty damage = full frame update
    let _ = state.tx.try_send(Frame::new(frame_data, width, height, vec![]));
}

pub struct PipeWireCapture {
    tx:   mpsc::SyncSender<Frame>,
    stop: Arc<AtomicBool>,
}

impl PipeWireCapture {
    pub fn new(tx: mpsc::SyncSender<Frame>, stop: Arc<AtomicBool>) -> Self {
        Self { tx, stop }
    }
}

impl CaptureBackend for PipeWireCapture {
    fn run(self: Box<Self>, _frame_duration: Duration) -> Result<()> {
        let state = Box::new(CallbackState {
            tx:   self.tx,
            rgba: std::cell::UnsafeCell::new(Vec::new()),
        });
        let state_ptr = Box::into_raw(state);

        // Map our Arc<AtomicBool> to a C volatile int
        let stop_int = AtomicI32::new(0);
        let stop_ref = &stop_int as *const AtomicI32 as *const libc::c_int;

        // Watcher thread: sets stop_int when stop flag fires
        let stop_clone = Arc::clone(&self.stop);
        let watcher = std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(50));
                if stop_clone.load(Ordering::Relaxed) {
                    stop_int.store(1, Ordering::SeqCst);
                    break;
                }
            }
        });

        let mut err_buf = vec![0i8; 256];
        let ret = unsafe {
            pw_capture_start(
                on_frame_cb,
                state_ptr as *mut libc::c_void,
                stop_ref,
                err_buf.as_mut_ptr(),
                err_buf.len() as libc::c_int,
            )
        };

        // Reclaim the state Box
        let _ = unsafe { Box::from_raw(state_ptr) };
        let _ = watcher.join();

        if ret < 0 {
            let msg = unsafe { std::ffi::CStr::from_ptr(err_buf.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            return Err(Error::PipeWire(msg));
        }
        Ok(())
    }
}
