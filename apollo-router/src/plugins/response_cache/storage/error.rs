use serde_json::error::Category;

use crate::plugins::response_cache::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("{0}")]
    Database(#[from] fred::error::Error),

    #[error("{0}")]
    Join(#[from] tokio::task::JoinError),

    #[error("NO_STORAGE")]
    NoStorage,

    #[error("{0}")]
    Serialize(#[from] serde_json::Error),

    #[error("TIMED_OUT")]
    Timeout(#[from] tokio::time::error::Elapsed),
}

impl Error {
    pub(crate) fn is_row_not_found(&self) -> bool {
        matches!(self, Error::Database(err) if err.is_not_found())
    }
}

impl ErrorCode for Error {
    fn code(&self) -> &'static str {
        const TIMEOUT_CODE: &str = "TIMEOUT";

        match self {
            Error::Database(err) => {
                if err.kind() == &fred::error::ErrorKind::Timeout || err.details() == "timeout" {
                    TIMEOUT_CODE
                } else {
                    err.kind().to_str()
                }
            }
            Error::Join(err) => {
                if err.is_cancelled() {
                    "CANCELLED"
                } else {
                    "PANICKED"
                }
            }
            Error::NoStorage => "NO_STORAGE",
            Error::Serialize(err) => match err.classify() {
                Category::Io => "Serialize::IO",
                Category::Syntax => "Serialize::Syntax",
                Category::Data => "Serialize::Data",
                Category::Eof => "Serialize::EOF",
            },
            Error::Timeout(_) => TIMEOUT_CODE,
        }
    }
}
