use std::fmt::Display;
use std::fmt::Formatter;

// TODO: look ahead to redis-rs error kinds
#[non_exhaustive]
#[derive(Debug, Clone, thiserror::Error)]
// TODO: remove allow(dead_code) - it's flagging bc of all the enum fields
#[allow(dead_code)]
pub(crate) enum Error {
    Ask(String),
    Authentication(String),
    Backpressure(String),
    Canceled(String),
    Cluster(String),
    Configuration(String),
    InvalidArgument(String),
    InvalidCommand(String),
    InvalidResponse(String),
    IO(String),
    Moved(String),
    Parse(String),
    Replica(String),
    Routing(String),
    Sentinel(String),
    Tls(String),
    Timeout,
    Unknown(String),
}

impl Error {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Ask(_) => "ask",
            Self::Authentication(_) => "auth",
            Self::Backpressure(_) => "backpressure",
            Self::Canceled(_) => "canceled",
            Self::Cluster(_) => "cluster",
            Self::Configuration(_) => "config",
            Self::InvalidArgument(_) => "invalid_argument",
            Self::InvalidCommand(_) => "invalid_command",
            Self::InvalidResponse(_) => "invalid_response",
            Self::IO(_) => "io",
            Self::Moved(_) => "moved",
            Self::Parse(_) => "parse",
            Self::Replica(_) => "replica",
            Self::Routing(_) => "routing",
            Self::Sentinel(_) => "sentinel",
            Self::Tls(_) => "tls",
            Self::Timeout => "timeout",
            Self::Unknown(_) => "unknown",
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl From<fred::error::Error> for Error {
    fn from(error: fred::error::Error) -> Self {
        use fred::error::ErrorKind;
        let details = error.details().to_string();

        match error.kind() {
            ErrorKind::Config => Error::Configuration(details),
            ErrorKind::Auth => Error::Authentication(details),
            ErrorKind::Routing => Error::Routing(details),
            ErrorKind::IO => Error::IO(details),
            ErrorKind::InvalidCommand => Error::InvalidCommand(details),
            ErrorKind::InvalidArgument => Error::InvalidArgument(details),
            ErrorKind::Url => Error::Configuration(details),
            ErrorKind::Protocol => Error::InvalidResponse(details),
            ErrorKind::Tls => Error::Tls(details),
            ErrorKind::Canceled => Error::Canceled(details),
            ErrorKind::Unknown if details == "timeout" => Error::Timeout,
            ErrorKind::Unknown => Error::Unknown(details),
            ErrorKind::Timeout => Error::Timeout,
            ErrorKind::Cluster if error.is_ask() => Error::Ask(details),
            ErrorKind::Cluster if error.is_moved() => Error::Moved(details),
            ErrorKind::Cluster => Error::Cluster(details),
            ErrorKind::Parse => Error::Parse(details),
            ErrorKind::Sentinel => Error::Sentinel(details),
            ErrorKind::Backpressure => Error::Backpressure(details),
            ErrorKind::Replica => Error::Replica(details),

            // TODO: hopefully we can get rid of this...
            ErrorKind::NotFound => {
                panic!("encountered not found error");
                //Error::Unknown(details)
            }
        }
    }
}

/// Record a Redis error as a metric, independent of having an active connection
pub(crate) fn record(error: &Error, caller: &'static str) {
    u64_counter_with_unit!(
        "apollo.router.cache.redis.errors",
        "Number of Redis errors by type",
        "{error}",
        1,
        kind = caller,
        error_type = error.code()
    );

    if !matches!(error, Error::Canceled(_)) {
        tracing::error!(
            error_type = error.code(),
            caller = caller,
            error = ?error,
            "Redis error occurred"
        );
    }
}
