use serde_json::error::Category;

use crate::plugins::response_cache::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("{0}")]
    Database(#[from] sqlx::Error),

    #[error("{0}")]
    Serialize(#[from] serde_json::Error),

    #[error("TIMED_OUT")]
    Timeout(#[from] tokio::time::error::Elapsed),
}

impl Error {
    pub(crate) fn is_row_not_found(&self) -> bool {
        match self {
            Error::Database(err) => matches!(err, &sqlx::Error::RowNotFound),
            Error::Serialize(_) | Error::Timeout(_) => false,
        }
    }
}

impl ErrorCode for Error {
    fn code(&self) -> &'static str {
        match self {
            Error::Database(err) => err.code(),
            Error::Serialize(err) => match err.classify() {
                Category::Io => "Serialize::IO",
                Category::Syntax => "Serialize::Syntax",
                Category::Data => "Serialize::Data",
                Category::Eof => "Serialize::EOF",
            },
            Error::Timeout(_) => "TIMED_OUT",
        }
    }
}
