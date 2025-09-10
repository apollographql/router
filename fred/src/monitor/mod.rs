use crate::{
  error::Error,
  types::{config::Config, Value},
  utils as client_utils,
};
use futures::Stream;
use std::fmt;

mod parser;
mod utils;

/// A command parsed from a [MONITOR](https://redis.io/commands/monitor) stream.
///
/// Formatting with the [Display](https://doc.rust-lang.org/std/fmt/trait.Display.html) trait will print the same output as `redis-cli`.
#[derive(Clone, Debug)]
pub struct MonitorCommand {
  /// The command run by the server.
  pub command:   String,
  /// Arguments passed to the command.
  pub args:      Vec<Value>,
  /// When the command was run on the server.
  pub timestamp: f64,
  /// The database against which the command was run.
  pub db:        u8,
  /// The host and port of the client that ran the command, or `lua` when run from a script.
  pub client:    String,
}

impl PartialEq for MonitorCommand {
  fn eq(&self, other: &Self) -> bool {
    client_utils::f64_eq(self.timestamp, other.timestamp)
      && self.client == other.client
      && self.db == other.db
      && self.command == other.command
      && self.args == other.args
  }
}

impl Eq for MonitorCommand {}

impl fmt::Display for MonitorCommand {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    write!(
      f,
      "{:.6} [{} {}] \"{}\"",
      self.timestamp, self.db, self.client, self.command
    )?;

    for arg in self.args.iter() {
      write!(f, " \"{}\"", arg.as_str().unwrap_or("unknown".into()))?;
    }

    Ok(())
  }
}

/// Run the [MONITOR](https://redis.io/commands/monitor) command against the provided server.
///
/// Currently only centralized configurations are supported.
pub async fn run(config: Config) -> Result<impl Stream<Item = MonitorCommand>, Error> {
  utils::start(config).await
}
