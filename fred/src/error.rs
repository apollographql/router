use bytes_utils::string::Utf8Error as BytesUtf8Error;
use futures::channel::oneshot::Canceled;
use redis_protocol::{error::RedisProtocolError, resp2::types::BytesFrame as Resp2Frame};
use semver::Error as SemverError;
use std::{
  borrow::{Borrow, Cow},
  convert::Infallible,
  fmt,
  io::Error as IoError,
  num::{ParseFloatError, ParseIntError},
  str,
  str::Utf8Error,
  string::FromUtf8Error,
};
use url::ParseError;

/// An enum representing the type of error.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ErrorKind {
  /// A fatal client configuration error. These errors will shut down a client and break out of any reconnection
  /// attempts.
  Config,
  /// An authentication error.
  Auth,
  /// An error finding a server that should receive a command.
  Routing,
  /// An IO error with the underlying connection.
  IO,
  /// An invalid command, such as trying to perform a `set` command on a client after calling `subscribe`.
  InvalidCommand,
  /// An invalid argument or set of arguments to a command.
  InvalidArgument,
  /// An invalid URL error.
  Url,
  /// A protocol error such as an invalid or unexpected frame from the server.
  Protocol,
  /// A TLS error.
  #[cfg(any(
    feature = "enable-native-tls",
    feature = "enable-rustls",
    feature = "enable-rustls-ring"
  ))]
  #[cfg_attr(
    docsrs,
    doc(cfg(any(
      feature = "enable-native-tls",
      feature = "enable-rustls",
      feature = "enable-rustls-ring"
    )))
  )]
  Tls,
  /// An error indicating the request was canceled.
  Canceled,
  /// An unknown error.
  Unknown,
  /// A timeout error.
  Timeout,
  /// An error used to indicate that the cluster's state has changed. These errors will show up on the `on_error`
  /// error stream even though the client will automatically attempt to recover.
  Cluster,
  /// A parser error.
  Parse,
  /// An error communicating with redis sentinel.
  Sentinel,
  /// An error indicating a value was not found, often used when trying to cast a `nil` response from the server to a
  /// non-nullable type.
  NotFound,
  /// An error indicating that the caller should apply backpressure and retry the command.
  Backpressure,
  /// An error associated with a replica node.
  #[cfg(feature = "replicas")]
  #[cfg_attr(docsrs, doc(cfg(feature = "replicas")))]
  Replica,
}

impl ErrorKind {
  pub fn to_str(&self) -> &'static str {
    match *self {
      ErrorKind::Auth => "Authentication Error",
      ErrorKind::IO => "IO Error",
      ErrorKind::Routing => "Routing Error",
      ErrorKind::InvalidArgument => "Invalid Argument",
      ErrorKind::InvalidCommand => "Invalid Command",
      ErrorKind::Url => "Url Error",
      ErrorKind::Protocol => "Protocol Error",
      ErrorKind::Unknown => "Unknown Error",
      ErrorKind::Canceled => "Canceled",
      ErrorKind::Cluster => "Cluster Error",
      ErrorKind::Timeout => "Timeout Error",
      #[cfg(any(
        feature = "enable-native-tls",
        feature = "enable-rustls",
        feature = "enable-rustls-ring"
      ))]
      ErrorKind::Tls => "TLS Error",
      ErrorKind::Config => "Config Error",
      ErrorKind::Parse => "Parse Error",
      ErrorKind::Sentinel => "Sentinel Error",
      ErrorKind::NotFound => "Not Found",
      ErrorKind::Backpressure => "Backpressure",
      #[cfg(feature = "replicas")]
      ErrorKind::Replica => "Replica",
    }
  }
}

/// An error from the server or client.
#[derive(Debug)]
pub struct Error {
  /// Details about the specific error condition.
  details: Cow<'static, str>,
  /// The kind of error.
  kind:    ErrorKind,
}

impl Clone for Error {
  fn clone(&self) -> Self {
    Error::new(self.kind.clone(), self.details.clone())
  }
}

impl PartialEq for Error {
  fn eq(&self, other: &Self) -> bool {
    self.kind == other.kind && self.details == other.details
  }
}

impl Eq for Error {}

impl fmt::Display for Error {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f, "{}: {}", self.kind.to_str(), self.details)
  }
}

#[doc(hidden)]
impl From<RedisProtocolError> for Error {
  fn from(e: RedisProtocolError) -> Self {
    Error::new(ErrorKind::Protocol, format!("{}", e))
  }
}

#[doc(hidden)]
impl From<()> for Error {
  fn from(_: ()) -> Self {
    Error::new(ErrorKind::Canceled, "Empty error.")
  }
}

#[doc(hidden)]
impl From<futures::channel::mpsc::SendError> for Error {
  fn from(e: futures::channel::mpsc::SendError) -> Self {
    Error::new(ErrorKind::Unknown, format!("{}", e))
  }
}

#[doc(hidden)]
impl From<tokio::sync::oneshot::error::RecvError> for Error {
  fn from(e: tokio::sync::oneshot::error::RecvError) -> Self {
    Error::new(ErrorKind::Unknown, format!("{}", e))
  }
}

#[doc(hidden)]
impl From<tokio::sync::broadcast::error::RecvError> for Error {
  fn from(e: tokio::sync::broadcast::error::RecvError) -> Self {
    Error::new(ErrorKind::Unknown, format!("{}", e))
  }
}

#[doc(hidden)]
impl<T: fmt::Display> From<tokio::sync::broadcast::error::SendError<T>> for Error {
  fn from(e: tokio::sync::broadcast::error::SendError<T>) -> Self {
    Error::new(ErrorKind::Unknown, format!("{}", e))
  }
}

#[doc(hidden)]
impl From<IoError> for Error {
  fn from(e: IoError) -> Self {
    Error::new(ErrorKind::IO, format!("{:?}", e))
  }
}

#[doc(hidden)]
impl From<ParseError> for Error {
  fn from(e: ParseError) -> Self {
    Error::new(ErrorKind::Url, format!("{:?}", e))
  }
}

#[doc(hidden)]
impl From<ParseFloatError> for Error {
  fn from(_: ParseFloatError) -> Self {
    Error::new(ErrorKind::Parse, "Invalid floating point number.")
  }
}

#[doc(hidden)]
impl From<ParseIntError> for Error {
  fn from(_: ParseIntError) -> Self {
    Error::new(ErrorKind::Parse, "Invalid integer string.")
  }
}

#[doc(hidden)]
impl From<FromUtf8Error> for Error {
  fn from(_: FromUtf8Error) -> Self {
    Error::new(ErrorKind::Parse, "Invalid UTF-8 string.")
  }
}

#[doc(hidden)]
impl From<Utf8Error> for Error {
  fn from(_: Utf8Error) -> Self {
    Error::new(ErrorKind::Parse, "Invalid UTF-8 string.")
  }
}

#[doc(hidden)]
impl<S> From<BytesUtf8Error<S>> for Error {
  fn from(e: BytesUtf8Error<S>) -> Self {
    e.utf8_error().into()
  }
}

#[doc(hidden)]
impl From<fmt::Error> for Error {
  fn from(e: fmt::Error) -> Self {
    Error::new(ErrorKind::Unknown, format!("{:?}", e))
  }
}

#[doc(hidden)]
impl From<Canceled> for Error {
  fn from(e: Canceled) -> Self {
    Error::new(ErrorKind::Canceled, format!("{}", e))
  }
}

#[doc(hidden)]
#[cfg(not(feature = "glommio"))]
impl From<tokio::task::JoinError> for Error {
  fn from(e: tokio::task::JoinError) -> Self {
    Error::new(ErrorKind::Unknown, format!("Spawn Error: {:?}", e))
  }
}

#[doc(hidden)]
#[cfg(feature = "glommio")]
impl<T: fmt::Debug> From<glommio::GlommioError<T>> for Error {
  fn from(e: glommio::GlommioError<T>) -> Self {
    Error::new(ErrorKind::Unknown, format!("{:?}", e))
  }
}

#[doc(hidden)]
#[cfg(feature = "glommio")]
impl From<oneshot::RecvError> for Error {
  fn from(_: oneshot::RecvError) -> Self {
    Error::new_canceled()
  }
}

#[doc(hidden)]
impl From<SemverError> for Error {
  fn from(e: SemverError) -> Self {
    Error::new(ErrorKind::Protocol, format!("Invalid server version: {:?}", e))
  }
}

#[doc(hidden)]
impl From<Infallible> for Error {
  fn from(e: Infallible) -> Self {
    warn!("Infallible error: {:?}", e);
    Error::new(ErrorKind::Unknown, "Unknown error.")
  }
}

#[doc(hidden)]
impl From<Resp2Frame> for Error {
  fn from(e: Resp2Frame) -> Self {
    match e {
      Resp2Frame::SimpleString(s) => match str::from_utf8(&s).ok() {
        Some("Canceled") => Error::new_canceled(),
        _ => Error::new(ErrorKind::Unknown, "Unknown frame error."),
      },
      _ => Error::new(ErrorKind::Unknown, "Unknown frame error."),
    }
  }
}

#[doc(hidden)]
#[cfg(feature = "enable-native-tls")]
#[cfg_attr(docsrs, doc(cfg(feature = "enable-native-tls")))]
impl From<native_tls::Error> for Error {
  fn from(e: native_tls::Error) -> Self {
    Error::new(ErrorKind::Tls, format!("{:?}", e))
  }
}

#[doc(hidden)]
#[cfg(any(feature = "enable-rustls", feature = "enable-rustls-ring"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "enable-rustls", feature = "enable-rustls-ring"))))]
impl From<rustls::pki_types::InvalidDnsNameError> for Error {
  fn from(e: rustls::pki_types::InvalidDnsNameError) -> Self {
    Error::new(ErrorKind::Tls, format!("{:?}", e))
  }
}

#[doc(hidden)]
#[cfg(any(feature = "enable-rustls", feature = "enable-rustls-ring"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "enable-rustls", feature = "enable-rustls-ring"))))]
impl From<rustls::Error> for Error {
  fn from(e: rustls::Error) -> Self {
    Error::new(ErrorKind::Tls, format!("{:?}", e))
  }
}

#[doc(hidden)]
#[cfg(feature = "trust-dns-resolver")]
#[cfg_attr(docsrs, doc(cfg(feature = "trust-dns-resolver")))]
impl From<trust_dns_resolver::error::ResolveError> for Error {
  fn from(e: trust_dns_resolver::error::ResolveError) -> Self {
    Error::new(ErrorKind::IO, format!("{:?}", e))
  }
}

#[doc(hidden)]
#[cfg(feature = "dns")]
#[cfg_attr(docsrs, doc(cfg(feature = "dns")))]
impl From<hickory_resolver::error::ResolveError> for Error {
  fn from(e: hickory_resolver::error::ResolveError) -> Self {
    Error::new(ErrorKind::IO, format!("{:?}", e))
  }
}

#[cfg(feature = "serde-json")]
#[cfg_attr(docsrs, doc(cfg(feature = "serde-json")))]
impl From<serde_json::Error> for Error {
  fn from(e: serde_json::Error) -> Self {
    Error::new(ErrorKind::Parse, format!("{:?}", e))
  }
}

impl Error {
  /// Create a new error with the provided details.
  pub fn new<T>(kind: ErrorKind, details: T) -> Error
  where
    T: Into<Cow<'static, str>>,
  {
    Error {
      kind,
      details: details.into(),
    }
  }

  /// Read the type of error without any associated data.
  pub fn kind(&self) -> &ErrorKind {
    &self.kind
  }

  /// Change the kind of the error.
  pub fn change_kind(&mut self, kind: ErrorKind) {
    self.kind = kind;
  }

  /// Read details about the error.
  pub fn details(&self) -> &str {
    self.details.borrow()
  }

  /// Create a new empty Canceled error.
  pub fn new_canceled() -> Self {
    Error::new(ErrorKind::Canceled, "Canceled.")
  }

  /// Create a new parse error with the provided details.
  pub(crate) fn new_parse<T>(details: T) -> Self
  where
    T: Into<Cow<'static, str>>,
  {
    Error::new(ErrorKind::Parse, details)
  }

  /// Create a new default backpressure error.
  pub(crate) fn new_backpressure() -> Self {
    Error::new(ErrorKind::Backpressure, "Max in-flight commands reached.")
  }

  /// Whether reconnection logic should be skipped in all cases.
  pub(crate) fn should_not_reconnect(&self) -> bool {
    matches!(self.kind, ErrorKind::Config | ErrorKind::Url)
  }

  /// Whether the error is a `Cluster` error.
  pub fn is_cluster(&self) -> bool {
    matches!(self.kind, ErrorKind::Cluster)
  }

  /// Whether the error is a `Canceled` error.
  pub fn is_canceled(&self) -> bool {
    matches!(self.kind, ErrorKind::Canceled)
  }

  /// Whether the error is a `Replica` error.
  #[cfg(feature = "replicas")]
  #[cfg_attr(docsrs, doc(cfg(feature = "replicas")))]
  pub fn is_replica(&self) -> bool {
    matches!(self.kind, ErrorKind::Replica)
  }

  /// Whether the error is a `NotFound` error.
  pub fn is_not_found(&self) -> bool {
    matches!(self.kind, ErrorKind::NotFound)
  }

  /// Whether the error is a MOVED redirection.
  pub fn is_moved(&self) -> bool {
    self.is_cluster() && self.details.starts_with("MOVED")
  }

  /// Whether the error is an ASK redirection.
  pub fn is_ask(&self) -> bool {
    self.is_cluster() && self.details.starts_with("ASK")
  }
}

impl std::error::Error for Error {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    None
  }
}
