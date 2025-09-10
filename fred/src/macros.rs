#![allow(unused_macros)]

macro_rules! to(
  ($val:ident) => {
    crate::utils::try_into($val)
  }
);

macro_rules! _trace(
  ($inner:tt, $($arg:tt)*) => { {
    if log::log_enabled!(log::Level::Trace) {
      log::trace!("{}: {}", $inner.id, format!($($arg)*))
    }
   } }
);

macro_rules! _debug(
  ($inner:tt, $($arg:tt)*) => { {
    if log::log_enabled!(log::Level::Debug) {
      log::debug!("{}: {}", $inner.id, format!($($arg)*))
    }
   } }
);

macro_rules! _error(
  ($inner:tt, $($arg:tt)*) => { {
    if log::log_enabled!(log::Level::Error) {
      log::error!("{}: {}", $inner.id, format!($($arg)*))
    }
   } }
);

macro_rules! _warn(
  ($inner:tt, $($arg:tt)*) => { {
    if log::log_enabled!(log::Level::Warn) {
      log::warn!("{}: {}", $inner.id, format!($($arg)*))
    }
   } }
);

macro_rules! _info(
  ($inner:tt, $($arg:tt)*) => { {
    if log::log_enabled!(log::Level::Info) {
      log::info!("{}: {}", $inner.id, format!($($arg)*))
    }
   } }
);

/// Span used within the client that uses the command's span ID as the parent.
#[cfg(any(feature = "full-tracing", feature = "partial-tracing"))]
macro_rules! fspan (
  ($cmd:ident, $lvl:expr, $($arg:tt)*) => { {
    let _id = $cmd.traces.cmd.as_ref().and_then(|c| c.id());
    span_lvl!($lvl, parent: _id, $($arg)*)
  } }
);

macro_rules! span_lvl {
    ($lvl:expr, $($args:tt)*) => {{
        match $lvl {
            tracing::Level::ERROR => tracing::error_span!($($args)*),
            tracing::Level::WARN => tracing::warn_span!($($args)*),
            tracing::Level::INFO => tracing::info_span!($($args)*),
            tracing::Level::DEBUG => tracing::debug_span!($($args)*),
            tracing::Level::TRACE => tracing::trace_span!($($args)*),
        }
    }};
}

/// Fake span used within the client that uses the command's span ID as the parent.
#[cfg(not(any(feature = "full-tracing", feature = "partial-tracing")))]
macro_rules! fspan (
  ($cmd:ident, $($arg:tt)*) => {
    crate::trace::Span {}
  }
);

/// Similar to `try`/`?`, but `continue` instead of breaking out with an error.  
macro_rules! try_or_continue (
  ($expr:expr) => {
    match $expr {
      Ok(val) => val,
      Err(_) => continue
    }
  }
);

/// A helper macro to wrap a string value in quotes via the [json](serde_json::json) macro.
///
/// See the [RedisJSON interface](crate::interfaces::RedisJsonInterface) for more information.
#[cfg(feature = "i-redis-json")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-redis-json")))]
#[macro_export]
macro_rules! json_quote(
  ($($json:tt)+) => {
    serde_json::json!($($json)+).to_string()
  }
);

/// Shorthand to create a [CustomCommand](crate::types::CustomCommand).
///
/// ```rust no_run
/// # use fred::{cmd, types::{CustomCommand, ClusterHash}};
/// let _cmd = cmd!("FOO.BAR");
/// let _cmd = cmd!("FOO.BAR", blocking: true);
/// let _cmd = cmd!("FOO.BAR", hash: ClusterHash::FirstKey);
/// let _cmd = cmd!("FOO.BAR", hash: ClusterHash::FirstKey, blocking: true);
/// // which is shorthand for
/// let _cmd = CustomCommand::new("FOO.BAR", ClusterHash::FirstKey, true);
/// ```
#[macro_export]
macro_rules! cmd(
  ($name:expr) => {
    fred::types::CustomCommand::new($name, fred::types::ClusterHash::FirstKey, false)
  };
  ($name:expr, blocking: $blk:expr) => {
    fred::types::CustomCommand::new($name, fred::types::ClusterHash::FirstKey, $blk)
  };
  ($name:expr, hash: $hash:expr) => {
    fred::types::CustomCommand::new($name, $hash, false)
  };
  ($name:expr, hash: $hash:expr, blocking: $blk:expr) => {
    fred::types::CustomCommand::new($name, $hash, $blk)
  };
);

macro_rules! static_val(
  ($val:expr) => {
    Value::from_static_str($val)
  }
);

macro_rules! into (
  ($val:ident) => (let $val = $val.into(););
  ($v1:ident, $v2:ident) => (
    let ($v1, $v2) = ($v1.into(), $v2.into());
  );
  ($v1:ident, $v2:ident, $v3:ident) => (
    let ($v1, $v2, $v3) = ($v1.into(), $v2.into(), $v3.into());
  );
  ($v1:ident, $v2:ident, $v3:ident, $v4:ident) => (
    let ($v1, $v2, $v3, $v4) = ($v1.into(), $v2.into(), $v3.into(), $v4.into());
  );
  ($v1:ident, $v2:ident, $v3:ident, $v4:ident, $v5:ident) => (
    let ($v1, $v2, $v3, $v4, $v5) = ($v1.into(), $v2.into(), $v3.into(), $v4.into(), $v5.into());
  );
  // add to this as needed
);

macro_rules! try_into (
  ($val:ident) => (let $val = to!($val)?;);
  ($v1:ident, $v2:ident) => (
    let ($v1, $v2) = (to!($v1)?, to!($v2)?);
  );
  ($v1:ident, $v2:ident, $v3:ident) => (
    let ($v1, $v2, $v3) = (to!($v1)?, to!($v2)?, to!($v3)?);
  );
  ($v1:ident, $v2:ident, $v3:ident, $v4:ident) => (
    let ($v1, $v2, $v3, $v4) = (to!($v1)?, to!($v2)?, to!($v3)?, to!($v4)?);
  );
  ($v1:ident, $v2:ident, $v3:ident, $v4:ident, $v5:ident) => (
    let ($v1, $v2, $v3, $v4, $v5) = (to!($v1)?, to!($v2)?, to!($v3)?, to!($v4)?, to!($v5)?);
  );
  // add to this as needed
);
