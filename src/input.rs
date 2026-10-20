//! Remote control: injects the client's mouse/keyboard events into the
//! compositor via `zwlr_virtual_pointer_v1` + `zwp_virtual_keyboard_v1`.
//! Only active with `--control` (see `Config::control`).
//!
//! Two layers: [`HeldState`] is pure bookkeeping (unit-tested); [`Injector`]
//! owns a dedicated Wayland connection and runs on its own thread, fed by
//! [`attach`] from the `input` DataChannel.

use std::collections::BTreeSet;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use wayland_client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1, zwp_virtual_keyboard_v1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1, zwlr_virtual_pointer_v1,
};
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;

use crate::error::{Error, Result};
use crate::protocol::InputEvent;

// ─── Layer 1: held-input bookkeeping ───────────────────────────────────────

/// xkb modifier mask bits of a standard evdev keymap.
pub const MOD_SHIFT: u32 = 1;
pub const MOD_LOCK: u32 = 2;
pub const MOD_CTRL: u32 = 4;
pub const MOD_ALT: u32 = 8;
pub const MOD_NUM: u32 = 16;
pub const MOD_LOGO: u32 = 64;
pub const MOD_ALTGR: u32 = 128;

const KEY_CAPSLOCK: u16 = 58;
const KEY_NUMLOCK: u16 = 69;
/// Highest evdev code accepted from the network (`KEY_MAX`).
const KEY_MAX: u16 = 0x2ff;

fn modifier_bit(code: u16) -> u32 {
    match code {
        42 | 54 => MOD_SHIFT,
        29 | 97 => MOD_CTRL,
        56 => MOD_ALT,
        100 => MOD_ALTGR,
        125 | 126 => MOD_LOGO,
        _ => 0,
    }
}

/// What to do with a key event after [`HeldState::key`] saw it.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// Invalid code, autorepeat, or release of an unheld key: send nothing.
    Drop,
    /// Forward the key; `Some((depressed, locked))` means the modifier state
    /// changed and must be sent too.
    Forward(Option<(u32, u32)>),
}

/// Tracks what the remote client is holding down, so (a) modifier state can
/// be sent with `zwp_virtual_keyboard_v1.modifiers` — a virtual keyboard's
/// modifiers are whatever its client says they are — and (b) everything can
/// be released when the session ends; otherwise a disconnect mid-keypress
/// leaves a key or mouse button stuck down on the host.
#[derive(Default)]
pub struct HeldState {
    keys: BTreeSet<u16>,
    buttons: BTreeSet<u32>,
    locked: u32,
}

impl HeldState {
    pub fn key(&mut self, code: u16, pressed: bool) -> KeyAction {
        if code == 0 || code > KEY_MAX {
            return KeyAction::Drop;
        }
        let before = (self.depressed(), self.locked);
        let changed = if pressed {
            self.keys.insert(code)
        } else {
            self.keys.remove(&code)
        };
        if !changed {
            return KeyAction::Drop;
        }
        if pressed {
            match code {
                KEY_CAPSLOCK => self.locked ^= MOD_LOCK,
                KEY_NUMLOCK => self.locked ^= MOD_NUM,
                _ => {}
            }
        }
        let after = (self.depressed(), self.locked);
        KeyAction::Forward((after != before).then_some(after))
    }

    /// False if redundant (press of a held button, release of an unheld one).
    pub fn button(&mut self, code: u32, pressed: bool) -> bool {
        if pressed {
            self.buttons.insert(code)
        } else {
            self.buttons.remove(&code)
        }
    }

    /// Drains everything still held: (keys, buttons). Locks are kept.
    pub fn release_all(&mut self) -> (Vec<u16>, Vec<u32>) {
        (
            std::mem::take(&mut self.keys).into_iter().collect(),
            std::mem::take(&mut self.buttons).into_iter().collect(),
        )
    }

    /// A modifier bit is set while ANY key mapping to it is held.
    pub fn depressed(&self) -> u32 {
        self.keys.iter().fold(0, |m, &k| m | modifier_bit(k))
    }

    pub fn locked(&self) -> u32 {
        self.locked
    }
}

/// Browser wheel pixels → `wl_pointer` axis value: one notch is ~100 px in
/// browsers and 15.0 in libinput.
pub fn wheel_to_axis(px: i16) -> f64 {
    f64::from(px) * 0.15
}

/// Discrete wheel steps (~100 px each), at least ±1 for any non-zero delta.
pub fn wheel_to_discrete(px: i16) -> i32 {
    let steps = (f64::from(px) / 100.0).round() as i32;
    if steps == 0 {
        i32::from(px.signum())
    } else {
        steps
    }
}

// ─── Layer 2: Wayland injector ─────────────────────────────────────────────

/// Normalised coordinate range of `InputEvent::PointerMotion`.
const MOTION_EXTENT: u32 = 65535;

#[derive(Default)]
struct Globals {
    seat: Option<wl_seat::WlSeat>,
    has_keyboard: bool,
    output: Option<wl_output::WlOutput>,
    pointer_manager: Option<(
        zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
        u32,
    )>,
    keyboard_manager: Option<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1>,
    keymap: Option<(u32, OwnedFd, u32)>,
}

pub struct Injector {
    conn: Connection,
    _queue: EventQueue<Globals>,
    pointer: zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    keyboard: Option<zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1>,
    held: HeldState,
    start: Instant,
}

fn wl_err(e: impl std::fmt::Display) -> Error {
    Error::Wayland(e.to_string())
}

impl Injector {
    /// Connects to `$WAYLAND_DISPLAY`. The pointer is bound to the FIRST
    /// advertised `wl_output` — the same rule the capture backend uses — so
    /// normalised coordinates land on the streamed output. The virtual
    /// keyboard reuses the compositor's own active keymap (from
    /// `wl_keyboard.keymap`), so key codes mean the same thing as on the
    /// physical keyboard.
    pub fn connect() -> Result<Self> {
        let conn = Connection::connect_to_env().map_err(wl_err)?;
        let mut queue = conn.new_event_queue::<Globals>();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut g = Globals::default();
        queue.roundtrip(&mut g).map_err(wl_err)?; // globals
        queue.roundtrip(&mut g).map_err(wl_err)?; // seat capabilities

        let seat = g
            .seat
            .clone()
            .ok_or_else(|| Error::Wayland("compositor lacks wl_seat".into()))?;
        let output = g.output.clone().ok_or(Error::NoOutput)?;
        let (pm, pm_version) = g.pointer_manager.clone().ok_or_else(|| {
            Error::Wayland("compositor lacks zwlr_virtual_pointer_manager_v1".into())
        })?;

        let pointer = if pm_version >= 2 {
            pm.create_virtual_pointer_with_output(Some(&seat), Some(&output), &qh, ())
        } else {
            log::warn!(
                "zwlr_virtual_pointer_manager_v1 v1: pointer not bound to the streamed output"
            );
            pm.create_virtual_pointer(Some(&seat), &qh, ())
        };

        let keyboard = match (g.keyboard_manager.clone(), g.has_keyboard) {
            (Some(km), true) => {
                let wl_kb = seat.get_keyboard(&qh, ());
                queue.roundtrip(&mut g).map_err(wl_err)?;
                if seat.version() >= 3 {
                    wl_kb.release();
                }
                match g.keymap.take() {
                    Some((format, fd, size)) => {
                        let vk = km.create_virtual_keyboard(&seat, &qh, ());
                        vk.keymap(format, fd.as_fd(), size);
                        Some(vk)
                    }
                    None => {
                        log::warn!("No keymap from the compositor, keyboard control disabled");
                        None
                    }
                }
            }
            (None, _) => {
                log::warn!(
                    "Compositor lacks zwp_virtual_keyboard_manager_v1, keyboard control disabled"
                );
                None
            }
            (Some(_), false) => {
                log::warn!("Seat has no keyboard, keyboard control disabled");
                None
            }
        };
        conn.flush().map_err(wl_err)?;

        Ok(Self {
            conn,
            _queue: queue,
            pointer,
            keyboard,
            held: HeldState::default(),
            start: Instant::now(),
        })
    }

    fn now(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    pub fn inject(&mut self, event: InputEvent) -> Result<()> {
        let t = self.now();
        match event {
            InputEvent::PointerMotion { x, y } => {
                self.pointer
                    .motion_absolute(t, x.into(), y.into(), MOTION_EXTENT, MOTION_EXTENT);
                self.pointer.frame();
            }
            InputEvent::PointerButton { button, pressed } => {
                let code = button.evdev_code();
                if self.held.button(code, pressed) {
                    let state = if pressed {
                        wl_pointer::ButtonState::Pressed
                    } else {
                        wl_pointer::ButtonState::Released
                    };
                    self.pointer.button(t, code, state);
                    self.pointer.frame();
                }
            }
            InputEvent::PointerAxis { dx, dy } => {
                self.pointer.axis_source(wl_pointer::AxisSource::Wheel);
                for (delta, axis) in [
                    (dy, wl_pointer::Axis::VerticalScroll),
                    (dx, wl_pointer::Axis::HorizontalScroll),
                ] {
                    if delta != 0 {
                        self.pointer.axis_discrete(
                            t,
                            axis,
                            wheel_to_axis(delta),
                            wheel_to_discrete(delta),
                        );
                    }
                }
                self.pointer.frame();
            }
            InputEvent::Key { code, pressed } => {
                let Some(kb) = &self.keyboard else {
                    return Ok(());
                };
                if let KeyAction::Forward(mods) = self.held.key(code, pressed) {
                    kb.key(t, code.into(), u32::from(pressed));
                    if let Some((depressed, locked)) = mods {
                        kb.modifiers(depressed, 0, locked, 0);
                    }
                }
            }
        }
        self.conn.flush().map_err(wl_err)
    }

    /// Releases every held key and button, then flushes.
    pub fn release_all(&mut self) -> Result<()> {
        let t = self.now();
        let (keys, buttons) = self.held.release_all();
        if let Some(kb) = &self.keyboard {
            for code in &keys {
                kb.key(t, (*code).into(), 0);
            }
            if !keys.is_empty() {
                kb.modifiers(0, 0, self.held.locked(), 0);
            }
        }
        for code in &buttons {
            self.pointer
                .button(t, *code, wl_pointer::ButtonState::Released);
        }
        if !buttons.is_empty() {
            self.pointer.frame();
        }
        self.conn.flush().map_err(wl_err)
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        let _ = self.release_all();
        self.pointer.destroy();
        if let Some(kb) = &self.keyboard {
            kb.destroy();
        }
        let _ = self.conn.flush();
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
    fn event(
        g: &mut Self,
        reg: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_seat" if g.seat.is_none() => g.seat = Some(reg.bind(name, version.min(7), qh, ())),
            "wl_output" if g.output.is_none() => g.output = Some(reg.bind(name, 1, qh, ())),
            "zwlr_virtual_pointer_manager_v1" => {
                let v = version.min(2);
                g.pointer_manager = Some((reg.bind(name, v, qh, ()), v));
            }
            "zwp_virtual_keyboard_manager_v1" => {
                g.keyboard_manager = Some(reg.bind(name, 1, qh, ()))
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Globals {
    fn event(
        g: &mut Self,
        _: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            g.has_keyboard = caps.contains(wl_seat::Capability::Keyboard);
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Globals {
    fn event(
        g: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Keymap { format, fd, size } = event {
            let format = match format {
                WEnum::Value(f) => f as u32,
                WEnum::Unknown(f) => f,
            };
            g.keymap = Some((format, fd, size));
        }
    }
}

macro_rules! ignore_events {
    ($($iface:ty),* $(,)?) => {$(
        impl Dispatch<$iface, ()> for Globals {
            fn event(_: &mut Self, _: &$iface, _: <$iface as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(
    wl_output::WlOutput,
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
);

// ─── Session glue ──────────────────────────────────────────────────────────

/// A running input session; call [`detach`](Self::detach) when the stream ends.
pub struct InputSession {
    channel: Arc<RTCDataChannel>,
    thread: std::thread::JoinHandle<()>,
}

/// Starts injecting events arriving on the `input` DataChannel.
pub fn attach(channel: &Arc<RTCDataChannel>) -> InputSession {
    let (tx, rx) = mpsc::channel::<InputEvent>();
    channel.on_message(Box::new(move |msg: DataChannelMessage| {
        if !msg.is_string {
            if let Some(event) = InputEvent::decode(&msg.data) {
                let _ = tx.send(event);
            }
        }
        Box::pin(async {})
    }));
    let thread = std::thread::Builder::new()
        .name("r2z-input".into())
        .spawn(move || run_injector(rx))
        .expect("failed to spawn input thread");
    InputSession {
        channel: Arc::clone(channel),
        thread,
    }
}

fn run_injector(rx: mpsc::Receiver<InputEvent>) {
    let mut injector = match Injector::connect() {
        Ok(i) => {
            log::info!("Remote control active");
            i
        }
        Err(e) => {
            log::error!("Remote control unavailable: {e}");
            while rx.recv().is_ok() {} // drain until the session ends
            return;
        }
    };
    while let Ok(event) = rx.recv() {
        if let Err(e) = injector.inject(event) {
            log::error!("Input injection failed, stopping remote control: {e}");
            return;
        }
    }
    // Dropping the injector releases anything still held.
}

impl InputSession {
    /// Stops accepting events and waits for the injector to release every
    /// held key/button.
    pub async fn detach(self) {
        // Replacing the handler drops the old closure and with it the only
        // sender, which ends the injector thread's receive loop.
        self.channel.on_message(Box::new(|_| Box::pin(async {})));
        let join = tokio::task::spawn_blocking(move || self.thread.join());
        if tokio::time::timeout(Duration::from_secs(2), join)
            .await
            .is_err()
        {
            log::warn!("Input thread did not stop within 2s");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fwd(mods: Option<(u32, u32)>) -> KeyAction {
        KeyAction::Forward(mods)
    }

    #[test]
    fn plain_keys_forward_without_modifier_updates() {
        let mut s = HeldState::default();
        assert_eq!(s.key(30, true), fwd(None));
        assert_eq!(s.key(30, false), fwd(None));
    }

    #[test]
    fn shift_sets_and_clears_its_bit() {
        let mut s = HeldState::default();
        assert_eq!(s.key(42, true), fwd(Some((MOD_SHIFT, 0))));
        assert_eq!(s.key(42, false), fwd(Some((0, 0))));
    }

    #[test]
    fn left_and_right_shift_overlap() {
        let mut s = HeldState::default();
        assert_eq!(s.key(42, true), fwd(Some((MOD_SHIFT, 0))));
        assert_eq!(s.key(54, true), fwd(None), "already shifted");
        assert_eq!(s.key(42, false), fwd(None), "right shift still held");
        assert_eq!(s.key(54, false), fwd(Some((0, 0))));
    }

    #[test]
    fn left_alt_and_altgr_are_distinct() {
        let mut s = HeldState::default();
        assert_eq!(s.key(56, true), fwd(Some((MOD_ALT, 0))));
        assert_eq!(s.key(100, true), fwd(Some((MOD_ALT | MOD_ALTGR, 0))));
    }

    #[test]
    fn caps_lock_toggles_on_press_only_and_survives_release_all() {
        let mut s = HeldState::default();
        assert_eq!(s.key(58, true), fwd(Some((0, MOD_LOCK))));
        assert_eq!(s.key(58, true), KeyAction::Drop, "autorepeat");
        assert_eq!(s.key(58, false), fwd(None));
        assert_eq!(s.key(58, true), fwd(Some((0, 0))));
        s.key(58, false);
        s.key(69, true);
        s.release_all();
        assert_eq!(s.locked(), MOD_NUM);
    }

    #[test]
    fn release_of_an_unheld_key_is_dropped() {
        assert_eq!(HeldState::default().key(30, false), KeyAction::Drop);
    }

    #[test]
    fn invalid_codes_are_dropped() {
        let mut s = HeldState::default();
        assert_eq!(s.key(0, true), KeyAction::Drop);
        assert_eq!(s.key(0x300, true), KeyAction::Drop);
        assert_eq!(s.key(0x2ff, true), fwd(None));
    }

    #[test]
    fn button_redundancy() {
        let mut s = HeldState::default();
        assert!(s.button(0x110, true));
        assert!(!s.button(0x110, true));
        assert!(s.button(0x110, false));
        assert!(!s.button(0x110, false));
    }

    #[test]
    fn release_all_drains_and_clears_modifiers() {
        let mut s = HeldState::default();
        s.key(42, true);
        s.key(30, true);
        s.button(0x111, true);
        assert_eq!(s.release_all(), (vec![30, 42], vec![0x111]));
        assert_eq!(s.depressed(), 0);
        assert_eq!(s.release_all(), (vec![], vec![]));
    }

    /// Needs a live compositor: `cargo test -- --ignored injector_connects`.
    /// Creates and destroys the virtual devices without sending any input.
    #[test]
    #[ignore]
    fn injector_connects_to_the_running_compositor() {
        let injector = Injector::connect().expect("connect");
        assert!(
            injector.keyboard.is_some(),
            "keyboard control should be available"
        );
    }

    #[test]
    fn wheel_conversion() {
        assert_eq!(wheel_to_axis(100), 15.0);
        assert_eq!(wheel_to_axis(-100), -15.0);
        assert_eq!(wheel_to_discrete(0), 0);
        assert_eq!(wheel_to_discrete(100), 1);
        assert_eq!(wheel_to_discrete(-250), -3, "round half away from zero");
        assert_eq!(wheel_to_discrete(30), 1);
        assert_eq!(wheel_to_discrete(-4), -1);
    }
}
