use std::fmt;

#[derive(Debug)]
pub enum Error {
    Wayland(String),
    NoScreencopyManager,
    NoOutput,
    FrameFailed,
    WebRTC(String),
    Io(std::io::Error),
    DmaBuf(String),
    PipeWire(String),
    ConsumerDisconnected,
    NoBackend,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wayland(e) => write!(f, "Wayland: {e}"),
            Self::NoScreencopyManager => write!(f, "zwlr_screencopy_manager_v1 не знайдено"),
            Self::NoOutput => write!(f, "wl_output не знайдено"),
            Self::FrameFailed => write!(f, "захоплення кадру провалилось"),
            Self::WebRTC(e) => write!(f, "WebRTC: {e}"),
            Self::Io(e) => write!(f, "IO: {e}"),
            Self::DmaBuf(e) => write!(f, "DMA-BUF: {e}"),
            Self::PipeWire(e) => write!(f, "PipeWire: {e}"),
            Self::ConsumerDisconnected => write!(f, "приймач кадрів відключився"),
            Self::NoBackend => write!(f, "немає доступного бекенду захоплення"),
        }
    }
}

impl std::error::Error for Error {}

impl From<webrtc::Error> for Error {
    fn from(e: webrtc::Error) -> Self {
        Self::WebRTC(e.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_formats_include_the_underlying_message() {
        assert_eq!(Error::WebRTC("boom".into()).to_string(), "WebRTC: boom");
        assert_eq!(Error::DmaBuf("x".into()).to_string(), "DMA-BUF: x");
        assert_eq!(Error::PipeWire("y".into()).to_string(), "PipeWire: y");
        assert_eq!(Error::Wayland("z".into()).to_string(), "Wayland: z");
    }

    #[test]
    fn variantless_errors_still_produce_readable_messages() {
        assert!(!Error::NoBackend.to_string().is_empty());
        assert!(!Error::NoScreencopyManager.to_string().is_empty());
        assert!(!Error::NoOutput.to_string().is_empty());
        assert!(!Error::FrameFailed.to_string().is_empty());
        assert!(!Error::ConsumerDisconnected.to_string().is_empty());
    }

    #[test]
    fn io_error_converts_via_from_and_keeps_its_message() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "nope");
        let err: Error = io_err.into();
        assert!(matches!(err, Error::Io(_)));
        assert!(err.to_string().contains("nope"));
    }
}
