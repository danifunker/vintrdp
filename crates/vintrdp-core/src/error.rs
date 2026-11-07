//! The protocol core's error type.

use std::fmt;
use std::io;

#[derive(Debug)]
pub enum Error {
    /// A PDU ended before a field it should hold.
    Truncated,
    /// The server sent something we can't make sense of.
    Protocol(String),
    /// The network or a local file failed.
    Io(io::Error),
    /// The user cancelled (closed the window, refused a certificate).
    Cancelled,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn protocol(msg: impl Into<String>) -> Error {
        Error::Protocol(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => f.write_str("truncated PDU"),
            Error::Protocol(m) => f.write_str(m),
            Error::Io(e) => write!(f, "{e}"),
            Error::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}

/// `Err(Error::Protocol(format!(...)))`.
#[macro_export]
macro_rules! bail {
    ($($arg:tt)*) => {
        return Err($crate::error::Error::Protocol(format!($($arg)*)))
    };
}
