//! An interface for mocking commands.
//!
//! There are several patterns for utilizing a mocking layer in tests. In some cases a simple "echo" interface is
//! enough, or in others callers may need to buffer a series of commands before performing any assertions, etc. More
//! complicated test scenarios may require storing and operating on real values.
//!
//! This interface exposes several interfaces and structs for supporting the above use cases:
//! * `Echo` - A simple mocking struct that returns the provided arguments back to the caller.
//! * `SimpleMap` - A mocking struct that implements the basic `GET`, `SET`, and `DEL` commands.
//! * `Buffer` - A mocking struct that buffers commands internally, returning `QUEUED` to each command. Callers can
//!   then drain or inspect the buffer later.
//!
//! The base `Mocks` trait is directly exposed so callers can implement their own mocking layer as well.

use crate::{
  error::{Error, ErrorKind},
  runtime::Mutex,
  types::{Key, Value},
};
use bytes_utils::Str;
use fred_macros::rm_send_if;
use glob_match::glob_match;
use std::{
  collections::{HashMap, VecDeque},
  fmt::Debug,
};

/// A wrapper type for the parts of an internal command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MockCommand {
  /// The first word in the command string. For example:
  /// * `SET` - `"SET"`
  /// * `XGROUP CREATE` - `"XGROUP"`
  /// * `INCRBY` - `"INCRBY"`
  pub cmd:        Str,
  /// The optional subcommand string (or second word) in the command string. For example:
  /// * `SET` - `None`
  /// * `XGROUP CREATE` - `Some("CREATE")`
  /// * `INCRBY` - `None`
  pub subcommand: Option<Str>,
  /// The ordered list of arguments to the command.
  pub args:       Vec<Value>,
}

/// An interface for intercepting and processing commands in a mocking layer.
#[allow(unused_variables)]
#[rm_send_if(feature = "glommio")]
pub trait Mocks: Debug + Send + Sync + 'static {
  /// Intercept and process a command, returning any `Value`.
  ///
  /// # Important
  ///
  /// The caller must ensure the response value makes sense in the context of the specific command(s) being mocked.
  /// The parsing logic following each command on the public interface will still be applied. __Most__ commands
  /// perform minimal parsing on the response, but some may require specific response formats to function correctly.
  ///
  /// `Value::Queued` can be used to return a value that will work almost anywhere.
  fn process_command(&self, command: MockCommand) -> Result<Value, Error>;

  /// Intercept and process an entire transaction. The provided commands will **not** include `EXEC`.
  ///
  /// Note: The default implementation redirects each command to the [process_command](Self::process_command)
  /// function. The results of each call are buffered and returned as an array.
  fn process_transaction(&self, commands: Vec<MockCommand>) -> Result<Value, Error> {
    let mut out = Vec::with_capacity(commands.len());

    for command in commands.into_iter() {
      out.push(self.process_command(command)?);
    }
    Ok(Value::Array(out))
  }
}

/// An implementation of a mocking layer that returns the provided arguments to the caller.
///
/// ```rust no_run
/// # use fred::prelude::*;
/// #[tokio::test]
/// async fn should_use_echo_mock() {
///   let config = Config {
///     mocks: Some(Arc::new(Echo)),
///     ..Default::default()
///   };
///   let client = Builder::from_config(config).build().unwrap();
///   client.init().await.expect("Failed to connect");
///
///   let actual: Vec<Value> = client
///     .set(
///       "foo",
///       "bar",
///       Some(Expiration::EX(100)),
///       Some(SetOptions::NX),
///       false,
///     )
///     .await
///     .expect("Failed to call SET");
///
///   let expected: Vec<Value> = vec![
///     "foo".into(),
///     "bar".into(),
///     "EX".into(),
///     100.into(),
///     "NX".into(),
///   ];
///   assert_eq!(actual, expected);
/// }
/// ```
#[derive(Debug)]
pub struct Echo;

impl Mocks for Echo {
  fn process_command(&self, command: MockCommand) -> Result<Value, Error> {
    Ok(Value::Array(command.args))
  }
}

/// A struct that implements some of the basic mapping functions. If callers require a mocking layer that stores and
/// operates on real values then this struct is a good place to start.
///
/// Note: This does **not** support expirations or `NX|XX` qualifiers.
///
/// ```rust no_run
/// #[tokio::test]
/// async fn should_use_echo_mock() {
///   let config = Config {
///     mocks: Some(Arc::new(SimpleMap::new())),
///     ..Default::default()
///   };
///   let client = Builder::from_config(config).build().unwrap();
///   client.init().await.expect("Failed to connect");
///
///   let actual: String = client
///       .set("foo", "bar", None, None, false)
///       .await
///       .expect("Failed to call SET");
///   assert_eq!(actual, "OK");
///
///   let actual: String = client.get("foo").await.expect("Failed to call GET");
///   assert_eq!(actual, "bar");
/// }
/// ```
#[derive(Debug)]
pub struct SimpleMap {
  values: Mutex<HashMap<Key, Value>>,
}

impl SimpleMap {
  /// Create a new empty `SimpleMap`.
  pub fn new() -> Self {
    SimpleMap {
      values: Mutex::new(HashMap::new()),
    }
  }

  /// Clear the inner map.
  pub fn clear(&self) {
    self.values.lock().clear();
  }

  /// Take the inner map.
  pub fn take(&self) -> HashMap<Key, Value> {
    self.values.lock().drain().collect()
  }

  /// Read a copy of the inner map.
  pub fn inner(&self) -> HashMap<Key, Value> {
    self.values.lock().iter().map(|(k, v)| (k.clone(), v.clone())).collect()
  }

  /// Perform a `GET` operation.
  pub fn get(&self, args: Vec<Value>) -> Result<Value, Error> {
    let key: Key = match args.first() {
      Some(key) => key.clone().try_into()?,
      None => return Err(Error::new(ErrorKind::InvalidArgument, "Missing key.")),
    };

    Ok(self.values.lock().get(&key).cloned().unwrap_or(Value::Null))
  }

  /// Perform a `SET` operation.
  pub fn set(&self, mut args: Vec<Value>) -> Result<Value, Error> {
    args.reverse();
    let key: Key = match args.pop() {
      Some(key) => key.try_into()?,
      None => return Err(Error::new(ErrorKind::InvalidArgument, "Missing key.")),
    };
    let value = match args.pop() {
      Some(value) => value,
      None => return Err(Error::new(ErrorKind::InvalidArgument, "Missing value.")),
    };

    let _ = self.values.lock().insert(key, value);
    Ok(Value::new_ok())
  }

  /// Perform a `DEL` operation.
  pub fn del(&self, args: Vec<Value>) -> Result<Value, Error> {
    let mut guard = self.values.lock();
    let mut count = 0;

    for arg in args.into_iter() {
      let key: Key = arg.try_into()?;
      if guard.remove(&key).is_some() {
        count += 1;
      }
    }

    Ok(count.into())
  }

  /// Perform a `SCAN` operation, returning all matching keys in one page.
  pub fn scan(&self, args: Vec<Value>) -> Result<Value, Error> {
    let match_idx = args.iter().enumerate().find_map(|(i, a)| {
      if let Some("MATCH") = a.as_str().as_ref().map(|s| s.as_ref()) {
        Some(i + 1)
      } else {
        None
      }
    });
    let pattern = match_idx.and_then(|i| args[i].as_string());

    let keys = self
      .values
      .lock()
      .keys()
      .filter_map(|k| {
        if let Some(pattern) = pattern.as_ref() {
          if let Some(_k) = k.as_str() {
            if glob_match(pattern, _k) {
              k.as_bytes_str().map(Value::String)
            } else {
              None
            }
          } else {
            None
          }
        } else {
          k.as_bytes_str().map(Value::String)
        }
      })
      .collect();
    Ok(Value::Array(vec![Value::from_static_str("0"), Value::Array(keys)]))
  }
}

impl Mocks for SimpleMap {
  fn process_command(&self, command: MockCommand) -> Result<Value, Error> {
    match &*command.cmd {
      "GET" => self.get(command.args),
      "SET" => self.set(command.args),
      "DEL" => self.del(command.args),
      "SCAN" => self.scan(command.args),
      _ => Err(Error::new(ErrorKind::Unknown, "Unimplemented.")),
    }
  }
}

/// A mocking layer that buffers the commands internally and returns `QUEUED` to the caller.
///
/// ```rust
/// #[tokio::test]
/// async fn should_use_buffer_mock() {
///   let buffer = Arc::new(Buffer::new());
///   let config = Config {
///     mocks: Some(buffer.clone()),
///     ..Default::default()
///   };
///   let client = Builder::from_config(config).build().unwrap();
///   client.init().await.expect("Failed to connect");
///
///   let actual: String = client
///     .set("foo", "bar", None, None, false)
///     .await
///     .expect("Failed to call SET");
///   assert_eq!(actual, "QUEUED");
///
///   let actual: String = client.get("foo").await.expect("Failed to call GET");
///   assert_eq!(actual, "QUEUED");
///
///   // note: values that act as keys use the `Value::Bytes` variant internally
///   let expected = vec![
///     MockCommand {
///       cmd:        "SET".into(),
///       subcommand: None,
///       args:       vec!["foo".as_bytes().into(), "bar".into()],
///     },
///     MockCommand {
///       cmd:        "GET".into(),
///       subcommand: None,
///       args:       vec!["foo".as_bytes().into()],
///     },
///   ];
///   assert_eq!(buffer.take(), expected);
/// }
/// ```
#[derive(Debug)]
pub struct Buffer {
  commands: Mutex<VecDeque<MockCommand>>,
}

impl Buffer {
  /// Create a new empty `Buffer`.
  pub fn new() -> Self {
    Buffer {
      commands: Mutex::new(VecDeque::new()),
    }
  }

  /// Read the length of the internal buffer.
  pub fn len(&self) -> usize {
    self.commands.lock().len()
  }

  /// Clear the inner buffer.
  pub fn clear(&self) {
    self.commands.lock().clear();
  }

  /// Drain and return the internal command buffer.
  pub fn take(&self) -> Vec<MockCommand> {
    self.commands.lock().drain(..).collect()
  }

  /// Read a copy of the internal command buffer without modifying the contents.
  pub fn inner(&self) -> Vec<MockCommand> {
    self.commands.lock().iter().cloned().collect()
  }

  /// Push a new command onto the back of the internal buffer.
  pub fn push_back(&self, command: MockCommand) {
    self.commands.lock().push_back(command);
  }

  /// Pop a command from the back of the internal buffer.
  pub fn pop_back(&self) -> Option<MockCommand> {
    self.commands.lock().pop_back()
  }

  /// Push a new command onto the front of the internal buffer.
  pub fn push_front(&self, command: MockCommand) {
    self.commands.lock().push_front(command);
  }

  /// Pop a command from the front of the internal buffer.
  pub fn pop_front(&self) -> Option<MockCommand> {
    self.commands.lock().pop_front()
  }
}

impl Mocks for Buffer {
  fn process_command(&self, command: MockCommand) -> Result<Value, Error> {
    self.push_back(command);
    Ok(Value::Queued)
  }
}

#[cfg(test)]
#[cfg(all(feature = "mocks", feature = "i-keys"))]
mod tests {
  use super::*;
  use crate::{
    clients::Client,
    error::Error,
    interfaces::{ClientLike, KeysInterface},
    mocks::{Buffer, Echo, Mocks, SimpleMap},
    prelude::Expiration,
    runtime::JoinHandle,
    types::{config::Config, scan::Scanner, SetOptions, Value},
  };
  use std::sync::Arc;
  use tokio_stream::StreamExt;

  async fn create_mock_client(mocks: Arc<dyn Mocks>) -> (Client, JoinHandle<Result<(), Error>>) {
    let config = Config {
      mocks: Some(mocks),
      ..Default::default()
    };
    let client = Client::new(config, None, None, None);
    let jh = client.connect();
    client.wait_for_connect().await.expect("Failed to connect");

    (client, jh)
  }

  #[tokio::test]
  async fn should_create_mock_config_and_client() {
    let _ = create_mock_client(Arc::new(Echo)).await;
  }

  #[tokio::test]
  async fn should_use_echo_mock() {
    let (client, _) = create_mock_client(Arc::new(Echo)).await;

    let actual: Vec<Value> = client
      .set("foo", "bar", Some(Expiration::EX(100)), Some(SetOptions::NX), false)
      .await
      .expect("Failed to call SET");

    let expected: Vec<Value> = vec!["foo".into(), "bar".into(), "EX".into(), 100.into(), "NX".into()];
    assert_eq!(actual, expected);
  }

  #[tokio::test]
  async fn should_use_simple_map_mock() {
    let (client, _) = create_mock_client(Arc::new(SimpleMap::new())).await;

    let actual: String = client
      .set("foo", "bar", None, None, false)
      .await
      .expect("Failed to call SET");
    assert_eq!(actual, "OK");

    let actual: String = client.get("foo").await.expect("Failed to call GET");
    assert_eq!(actual, "bar");
  }

  #[tokio::test]
  async fn should_use_buffer_mock() {
    let buffer = Arc::new(Buffer::new());
    let (client, _) = create_mock_client(buffer.clone()).await;

    let actual: String = client
      .set("foo", "bar", None, None, false)
      .await
      .expect("Failed to call SET");
    assert_eq!(actual, "QUEUED");

    let actual: String = client.get("foo").await.expect("Failed to call GET");
    assert_eq!(actual, "QUEUED");

    let expected = vec![
      MockCommand {
        cmd:        "SET".into(),
        subcommand: None,
        args:       vec!["foo".as_bytes().into(), "bar".into()],
      },
      MockCommand {
        cmd:        "GET".into(),
        subcommand: None,
        args:       vec!["foo".as_bytes().into()],
      },
    ];
    assert_eq!(buffer.take(), expected);
  }

  #[tokio::test]
  async fn should_mock_pipelines() {
    let (client, _) = create_mock_client(Arc::new(Echo)).await;

    let pipeline = client.pipeline();
    pipeline.get::<(), _>("foo").await.unwrap();
    pipeline.get::<(), _>("bar").await.unwrap();

    let all: Vec<Vec<String>> = pipeline.all().await.unwrap();
    assert_eq!(all, vec![vec!["foo"], vec!["bar"]]);
    let try_all = pipeline.try_all::<Vec<String>>().await;
    assert_eq!(try_all, vec![Ok(vec!["foo".to_string()]), Ok(vec!["bar".to_string()])]);
    let last: Vec<String> = pipeline.last().await.unwrap();
    assert_eq!(last, vec!["bar"]);
  }

  #[tokio::test]
  async fn should_mock_scans() {
    let (client, _) = create_mock_client(Arc::new(SimpleMap::new())).await;
    client
      .set::<(), _, _>("foo1", "bar1", None, None, false)
      .await
      .expect("Failed to call SET");
    client
      .set::<(), _, _>("foo2", "bar2", None, None, false)
      .await
      .expect("Failed to call SET");
    let mut all: Vec<String> = Vec::new();
    let mut scan_stream = client.scan("foo*", Some(10), None);
    while let Some(mut page) = scan_stream.try_next().await.expect("failed to call try_next") {
      if let Some(keys) = page.take_results() {
        all.append(
          &mut keys
            .into_iter()
            .filter_map(|v| v.as_str().map(|v| v.to_string()))
            .collect(),
        );
      }
      page.next();
    }
    all.sort();
    assert_eq!(all, vec!["foo1".to_string(), "foo2".to_string()]);
  }

  #[tokio::test]
  async fn should_mock_scans_buffered() {
    let (client, _) = create_mock_client(Arc::new(SimpleMap::new())).await;
    client
      .set::<(), _, _>("foo1", "bar1", None, None, false)
      .await
      .expect("Failed to call SET");
    client
      .set::<(), _, _>("foo2", "bar2", None, None, false)
      .await
      .expect("Failed to call SET");

    let mut keys: Vec<String> = client
      .scan_buffered("foo*", Some(10), None)
      .map(|k| k.map(|k| k.into_string().unwrap()))
      .collect::<Result<Vec<String>, Error>>()
      .await
      .unwrap();
    keys.sort();

    assert_eq!(keys, vec!["foo1".to_string(), "foo2".to_string()]);
  }
}
