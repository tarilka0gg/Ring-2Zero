//! Binary wire format of the WebRTC DataChannels. Pure encode/decode, no
//! I/O — `docs/client-examples/client.html` is the other half of this file.
//!
//! All integers are little-endian.
//!
//! Server → client, `screen` channel:
//! - resolution header, 6 bytes: `0xFFFF | width u16 | height u16`
//! - sequence packet, 10 bytes: `0xFFFE | seq u32 | tile_count u32`
//! - tile packets: back-to-back `tile_len u32 | x u16 | y u16 | w u16 | h u16 | webp`,
//!   where `tile_len = 8 + webp.len()`, up to [`MAX_PACKET_SIZE`] per packet
//!
//! Client → server, `screen` channel: ACK, exactly 4 bytes: `seq u32`.
//!
//! Client → server, `input` channel: one [`InputEvent`] per message.

use bytes::Bytes;

pub const MARKER_HEADER: u16 = 0xFFFF;
pub const MARKER_SEQ: u16 = 0xFFFE;
/// Tiles are packed into packets up to this size; a tile larger than this
/// on its own gets a packet to itself. Tiles are never split.
pub const MAX_PACKET_SIZE: usize = 8_000;

const TILE_PREFIX: usize = 8;

/// A tile's placement on screen, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Resolution header, or `None` if a dimension doesn't fit the protocol's u16.
pub fn encode_header(width: u32, height: u32) -> Option<[u8; 6]> {
    let w = u16::try_from(width).ok()?;
    let h = u16::try_from(height).ok()?;
    let mut buf = [0u8; 6];
    buf[0..2].copy_from_slice(&MARKER_HEADER.to_le_bytes());
    buf[2..4].copy_from_slice(&w.to_le_bytes());
    buf[4..6].copy_from_slice(&h.to_le_bytes());
    Some(buf)
}

/// Sequence packet announcing the next `tile_count` tiles as batch `seq`.
pub fn encode_seq(seq: u32, tile_count: u32) -> [u8; 10] {
    let mut buf = [0u8; 10];
    buf[0..2].copy_from_slice(&MARKER_SEQ.to_le_bytes());
    buf[2..6].copy_from_slice(&seq.to_le_bytes());
    buf[6..10].copy_from_slice(&tile_count.to_le_bytes());
    buf
}

/// Client ACK for batch `seq`; anything that isn't exactly 4 bytes isn't one.
pub fn decode_ack(msg: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(msg.try_into().ok()?))
}

/// Packs `(rect, webp)` tiles, in order, into DataChannel packets. Rects
/// must fit in u16 — callers guarantee that via [`encode_header`].
pub fn pack_tiles<'a>(tiles: impl IntoIterator<Item = (TileRect, &'a [u8])>) -> Vec<Bytes> {
    let mut packets = Vec::new();
    let mut current: Vec<u8> = Vec::new();

    for (rect, webp) in tiles {
        let tile_len = TILE_PREFIX + webp.len();
        let encoded_len = 4 + tile_len;

        if !current.is_empty() && current.len() + encoded_len > MAX_PACKET_SIZE {
            packets.push(Bytes::from(std::mem::take(&mut current)));
        }

        current.reserve(encoded_len);
        current.extend_from_slice(&(tile_len as u32).to_le_bytes());
        for v in [rect.x, rect.y, rect.width, rect.height] {
            current.extend_from_slice(&(v as u16).to_le_bytes());
        }
        current.extend_from_slice(webp);

        // Only an oversized tile alone can push past the limit — ship it now
        // so nothing gets appended after it.
        if current.len() > MAX_PACKET_SIZE {
            packets.push(Bytes::from(std::mem::take(&mut current)));
        }
    }

    if !current.is_empty() {
        packets.push(Bytes::from(current));
    }
    packets
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Back,
    Forward,
}

impl MouseButton {
    /// Browser `MouseEvent.button` numbering.
    fn from_wire(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Left,
            1 => Self::Middle,
            2 => Self::Right,
            3 => Self::Back,
            4 => Self::Forward,
            _ => return None,
        })
    }

    fn to_wire(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
            Self::Back => 3,
            Self::Forward => 4,
        }
    }

    /// Linux `input-event-codes.h` value (`BTN_LEFT` …).
    pub fn evdev_code(self) -> u32 {
        match self {
            Self::Left => 0x110,
            Self::Right => 0x111,
            Self::Middle => 0x112,
            Self::Back => 0x113,
            Self::Forward => 0x114,
        }
    }
}

/// A remote-control event from the client's `input` channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// Absolute position in screen pixels. `0x01 | x u16 | y u16`
    PointerMotion { x: u16, y: u16 },
    /// `0x02 | button u8 | pressed u8`
    PointerButton { button: MouseButton, pressed: bool },
    /// Scroll delta in pixels. `0x03 | dx i16 | dy i16`
    PointerAxis { dx: i16, dy: i16 },
    /// Linux evdev keycode. `0x04 | code u16 | pressed u8`
    Key { code: u16, pressed: bool },
}

fn wire_bool(v: u8) -> Option<bool> {
    match v {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

impl InputEvent {
    /// Decodes untrusted network input; malformed messages yield `None`.
    pub fn decode(msg: &[u8]) -> Option<Self> {
        let u16_at = |i: usize| u16::from_le_bytes([msg[i], msg[i + 1]]);
        match (msg.first()?, msg.len()) {
            (0x01, 5) => Some(Self::PointerMotion { x: u16_at(1), y: u16_at(3) }),
            (0x02, 3) => Some(Self::PointerButton {
                button: MouseButton::from_wire(msg[1])?,
                pressed: wire_bool(msg[2])?,
            }),
            (0x03, 5) => Some(Self::PointerAxis { dx: u16_at(1) as i16, dy: u16_at(3) as i16 }),
            (0x04, 4) => Some(Self::Key { code: u16_at(1), pressed: wire_bool(msg[3])? }),
            _ => None,
        }
    }

    /// Inverse of [`decode`](Self::decode).
    pub fn encode(&self) -> Vec<u8> {
        match *self {
            Self::PointerMotion { x, y } => [&[0x01][..], &x.to_le_bytes(), &y.to_le_bytes()].concat(),
            Self::PointerButton { button, pressed } => vec![0x02, button.to_wire(), pressed as u8],
            Self::PointerAxis { dx, dy } => [&[0x03][..], &dx.to_le_bytes(), &dy.to_le_bytes()].concat(),
            Self::Key { code, pressed } => [&[0x04][..], &code.to_le_bytes(), &[pressed as u8]].concat(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u32) -> TileRect {
        TileRect { x, y: 2 * x, width: 64, height: 36 }
    }

    /// Parses packets back into (rect, payload) — mirrors the client decoder.
    fn unpack(packets: &[Bytes]) -> Vec<(TileRect, Vec<u8>)> {
        let mut out = Vec::new();
        for p in packets {
            let mut off = 0;
            while off < p.len() {
                let len = u32::from_le_bytes(p[off..off + 4].try_into().unwrap()) as usize;
                let t = &p[off + 4..off + 4 + len];
                let f = |i: usize| u16::from_le_bytes([t[i], t[i + 1]]) as u32;
                out.push((TileRect { x: f(0), y: f(2), width: f(4), height: f(6) }, t[8..].to_vec()));
                off += 4 + len;
            }
        }
        out
    }

    #[test]
    fn header_bytes_and_limit() {
        assert_eq!(encode_header(1920, 1080), Some([0xFF, 0xFF, 0x80, 0x07, 0x38, 0x04]));
        assert_eq!(encode_header(65535, 1), Some([0xFF, 0xFF, 0xFF, 0xFF, 0x01, 0x00]));
        assert_eq!(encode_header(65536, 1080), None);
        assert_eq!(encode_header(1920, 65536), None);
    }

    #[test]
    fn seq_packet_bytes() {
        assert_eq!(encode_seq(0x0102_0304, 7), [0xFE, 0xFF, 0x04, 0x03, 0x02, 0x01, 7, 0, 0, 0]);
    }

    #[test]
    fn ack_is_exactly_four_bytes() {
        assert_eq!(decode_ack(&0x1234_5678u32.to_le_bytes()), Some(0x1234_5678));
        assert_eq!(decode_ack(&[1, 2, 3]), None);
        assert_eq!(decode_ack(&[1, 2, 3, 4, 5]), None);
        assert_eq!(decode_ack(&[]), None);
    }

    #[test]
    fn no_tiles_no_packets() {
        assert!(pack_tiles(std::iter::empty()).is_empty());
    }

    #[test]
    fn two_small_tiles_share_one_packet_byte_exact() {
        let packets = pack_tiles([(rect(1), &b"ab"[..]), (rect(3), &b"c"[..])]);
        assert_eq!(packets.len(), 1);
        let expected: Vec<u8> = [
            &10u32.to_le_bytes()[..], &[1, 0, 2, 0, 64, 0, 36, 0], b"ab",
            &9u32.to_le_bytes()[..], &[3, 0, 6, 0, 64, 0, 36, 0], b"c",
        ]
        .concat();
        assert_eq!(&packets[0][..], &expected[..]);
    }

    #[test]
    fn many_tiles_split_without_exceeding_the_limit() {
        let payload = vec![7u8; 2000]; // 2012 bytes encoded → 3 per packet
        let tiles: Vec<_> = (0..5).map(|i| (rect(i), &payload[..])).collect();
        let packets = pack_tiles(tiles.clone());
        assert_eq!(packets.len(), 2);
        assert!(packets.iter().all(|p| p.len() <= MAX_PACKET_SIZE));
        let back = unpack(&packets);
        assert_eq!(back.len(), 5);
        for ((r, p), (r2, p2)) in tiles.iter().zip(&back) {
            assert_eq!((r, *p), (r2, &p2[..]));
        }
    }

    #[test]
    fn tile_exactly_filling_a_packet_is_not_split_off() {
        let payload = vec![0u8; MAX_PACKET_SIZE - 12];
        let packets = pack_tiles([(rect(0), &payload[..]), (rect(1), &b"x"[..])]);
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0].len(), MAX_PACKET_SIZE);
    }

    #[test]
    fn oversized_tile_gets_its_own_packet_in_order() {
        let big = vec![9u8; 9000];
        let tiles = [(rect(0), &b"small"[..]), (rect(1), &big[..]), (rect(2), &b"tail"[..])];
        let packets = pack_tiles(tiles);
        assert_eq!(packets.iter().map(|p| p.len()).collect::<Vec<_>>(), [17, 9012, 16]);
        let back = unpack(&packets);
        assert_eq!(back.iter().map(|(r, _)| r.x).collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(back[1].1, big);
    }

    #[test]
    fn input_events_round_trip() {
        let events = [
            InputEvent::PointerMotion { x: 1919, y: 1079 },
            InputEvent::PointerButton { button: MouseButton::Right, pressed: true },
            InputEvent::PointerButton { button: MouseButton::Forward, pressed: false },
            InputEvent::PointerAxis { dx: -5, dy: i16::MIN },
            InputEvent::Key { code: 42, pressed: false },
            InputEvent::Key { code: 0x1ff, pressed: true },
        ];
        for e in events {
            assert_eq!(InputEvent::decode(&e.encode()), Some(e), "{e:?}");
        }
    }

    #[test]
    fn malformed_input_is_rejected() {
        for bad in [
            &[][..],
            &[0x09],
            &[0x01, 0x64, 0x00, 0xC8],
            &[0x04, 0x2A, 0x00, 0x01, 0x00],
            &[0x02, 0x00, 0x02],
            &[0x04, 0x2A, 0x00, 0x02],
            &[0x02, 0x05, 0x01],
        ] {
            assert_eq!(InputEvent::decode(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn mouse_button_evdev_codes() {
        use MouseButton::*;
        let codes: Vec<u32> = [Left, Right, Middle, Back, Forward].map(MouseButton::evdev_code).into();
        assert_eq!(codes, [0x110, 0x111, 0x112, 0x113, 0x114]);
    }
}
