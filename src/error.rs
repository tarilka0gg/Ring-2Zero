use std::fmt;

#[derive(Debug)]
pub enum Error {
    Wayland(String),
    NoScreencopyManager,
    NoOutput,
    FrameFailed,
    WebRTC(String),
    Io(std::io::Error),
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
