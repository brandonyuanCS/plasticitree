use std::{fmt, io};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Corruption(String),
    InvalidInput(String),
    Locked,
    RecoveryRequired,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "storage IO failed: {e}"),
            Self::Corruption(e) => write!(f, "corrupt database: {e}"),
            Self::InvalidInput(e) => write!(f, "invalid input: {e}"),
            Self::Locked => f.write_str("database is already open"),
            Self::RecoveryRequired => f.write_str("close and reopen the database to recover"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

pub(crate) fn corrupt(message: impl Into<String>) -> Error {
    Error::Corruption(message.into())
}
