use crate::{
  error::ErrorKind,
  modules::inner::ClientInner,
  prelude::Error,
  protocol::{
    command::Command,
    connection,
    connection::Connection,
    responders::{self, ResponseKind},
  },
  router::Connections,
  runtime::RefCount,
  types::config::ServerConfig,
};
use redis_protocol::resp3::types::{BytesFrame as Resp3Frame, Resp3Frame as _Resp3Frame};
use std::collections::VecDeque;

/// Process the response frame in the context of the last command.
///
/// Errors returned here will be logged, but will not close the socket or initiate a reconnect.
#[inline(always)]
pub fn process_response_frame(
  inner: &RefCount<ClientInner>,
  conn: &mut Connection,
  frame: Resp3Frame,
) -> Result<(), Error> {
  _trace!(inner, "Parsing response frame from {}", conn.server);
  let mut command = match conn.buffer.pop_front() {
    Some(command) => command,
    None => {
      _debug!(
        inner,
        "Missing last command from {}. Dropping {:?}.",
        conn.server,
        frame.kind()
      );
      return Ok(());
    },
  };
  _trace!(
    inner,
    "Checking response to {} ({})",
    command.kind.to_str_debug(),
    command.debug_id()
  );
  if command.blocks_connection() {
    conn.blocked = false;
    inner.backchannel.set_unblocked();
  }
  #[cfg(feature = "partial-tracing")]
  let _ = command.traces.network.take();

  _trace!(inner, "Handling centralized response kind: {:?}", command.response);
  match command.take_response() {
    ResponseKind::Skip | ResponseKind::Respond(None) => Ok(()),
    ResponseKind::Respond(Some(tx)) => responders::respond_to_caller(inner, &conn.server, command, tx, frame),
    ResponseKind::Buffer {
      received,
      expected,
      frames,
      tx,
      index,
      error_early,
    } => responders::respond_buffer(
      inner,
      &conn.server,
      command,
      received,
      expected,
      error_early,
      frames,
      index,
      tx,
      frame,
    ),
    ResponseKind::KeyScan(scanner) => responders::respond_key_scan(inner, &conn.server, command, scanner, frame),
    ResponseKind::ValueScan(scanner) => responders::respond_value_scan(inner, &conn.server, command, scanner, frame),
    ResponseKind::KeyScanBuffered(scanner) => {
      responders::respond_key_scan_buffered(inner, &conn.server, command, scanner, frame)
    },
  }
}

/// Initialize fresh connections to the server, dropping any old connections and saving in-flight commands on
/// `buffer`.
pub async fn initialize_connection(
  inner: &RefCount<ClientInner>,
  connections: &mut Connections,
  buffer: &mut VecDeque<Command>,
) -> Result<(), Error> {
  _debug!(inner, "Initializing centralized connection.");
  buffer.extend(connections.disconnect_all(inner).await);

  match connections {
    Connections::Centralized { connection: writer, .. } => {
      let server = match inner.config.server {
        ServerConfig::Centralized { ref server } => server.clone(),
        #[cfg(feature = "unix-sockets")]
        ServerConfig::Unix { ref path } => path.as_path().into(),
        _ => return Err(Error::new(ErrorKind::Config, "Expected centralized config.")),
      };
      let mut transport = connection::create(inner, &server, None).await?;
      transport.setup(inner, None).await?;
      let connection = transport.into_pipelined(false);
      inner.notifications.broadcast_reconnect(server);

      *writer = Some(connection);
      Ok(())
    },
    _ => Err(Error::new(ErrorKind::Config, "Expected centralized connection.")),
  }
}
