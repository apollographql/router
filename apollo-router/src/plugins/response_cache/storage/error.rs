use std::fmt::Display;
use std::fmt::Formatter;

use serde_json::error::Category;

use crate::plugins::response_cache::ErrorCode;

#[derive(Debug)]
pub(crate) enum Error {
    Database(sqlx::Error),
    Serialize(serde_json::Error),
    Timeout,
}

impl Error {
    pub(crate) fn is_row_not_found(&self) -> bool {
        match self {
            Error::Database(err) => matches!(err, &sqlx::Error::RowNotFound),
            Error::Serialize(_) => false,
            Error::Timeout => false,
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Database(err) => f.write_str(&err.to_string()),
            Error::Serialize(err) => f.write_str(&err.to_string()),
            Error::Timeout => f.write_str("TIMED_OUT"),
        }
    }
}

impl From<sqlx::Error> for Error {
    fn from(err: sqlx::Error) -> Self {
        Error::Database(err)
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::Serialize(err)
    }
}

impl From<tokio::time::error::Elapsed> for Error {
    fn from(_: tokio::time::error::Elapsed) -> Self {
        Error::Timeout
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
            Error::Timeout => "TIMED_OUT",
        }
    }
}

impl std::error::Error for Error {}
