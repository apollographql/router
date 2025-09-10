use crate::{
  error::{Error, ErrorKind},
  modules::inner::ClientInner,
  monitor::{parser, MonitorCommand},
  protocol::{
    codec::Codec,
    command::{Command, CommandKind},
    connection::{self, ConnectionKind, ExclusiveConnection},
    types::ProtocolFrame,
    utils as protocol_utils,
  },
  runtime::{channel, spawn, RefCount, Sender},
  types::config::{Config, ConnectionConfig, PerformanceConfig, ServerConfig},
};
use futures::stream::{Peekable, Stream, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::Framed;

#[cfg(all(feature = "blocking-encoding", not(feature = "glommio")))]
use redis_protocol::resp3::types::Resp3Frame;

#[cfg(all(feature = "blocking-encoding", not(feature = "glommio")))]
async fn handle_monitor_frame(
  inner: &RefCount<ClientInner>,
  frame: Result<ProtocolFrame, Error>,
) -> Option<MonitorCommand> {
  let frame = match frame {
    Ok(frame) => frame.into_resp3(),
    Err(e) => {
      _error!(inner, "Error on monitor stream: {:?}", e);
      return None;
    },
  };
  let frame_size = frame.encode_len(true);

  if frame_size >= inner.with_perf_config(|c| c.blocking_encode_threshold) {
    // since this isn't called from the Encoder/Decoder trait we can use spawn_blocking here
    _trace!(
      inner,
      "Parsing monitor frame with blocking task with size {}",
      frame_size
    );

    let inner = inner.clone();
    tokio::task::spawn_blocking(move || parser::parse(&inner, frame))
      .await
      .ok()
      .flatten()
  } else {
    parser::parse(inner, frame)
  }
}

#[cfg(any(not(feature = "blocking-encoding"), feature = "glommio"))]
async fn handle_monitor_frame(
  inner: &RefCount<ClientInner>,
  frame: Result<ProtocolFrame, Error>,
) -> Option<MonitorCommand> {
  let frame = match frame {
    Ok(frame) => frame.into_resp3(),
    Err(e) => {
      _error!(inner, "Error on monitor stream: {:?}", e);
      return None;
    },
  };

  parser::parse(inner, frame)
}

async fn send_monitor_command(
  inner: &RefCount<ClientInner>,
  mut connection: ExclusiveConnection,
) -> Result<ExclusiveConnection, Error> {
  _debug!(inner, "Sending MONITOR command.");

  let command = Command::new(CommandKind::Monitor, vec![]);
  let frame = connection.request_response(command, inner.is_resp3()).await?;

  _trace!(inner, "Recv MONITOR response: {:?}", frame);
  let response = protocol_utils::frame_to_results(frame)?;
  protocol_utils::expect_ok(&response)?;
  Ok(connection)
}

async fn forward_results<T>(
  inner: &RefCount<ClientInner>,
  tx: Sender<MonitorCommand>,
  mut framed: Peekable<Framed<T, Codec>>,
) where
  T: AsyncRead + AsyncWrite + Unpin + 'static,
{
  while let Some(frame) = framed.next().await {
    if let Some(command) = handle_monitor_frame(inner, frame).await {
      if let Err(_) = tx.try_send(command) {
        _warn!(inner, "Stopping monitor stream.");
        return;
      }
    } else {
      _debug!(inner, "Skipping invalid monitor frame.");
    }
  }
}

async fn process_stream(inner: &RefCount<ClientInner>, tx: Sender<MonitorCommand>, connection: ExclusiveConnection) {
  _debug!(inner, "Starting monitor stream processing...");

  match connection.transport {
    ConnectionKind::Tcp(framed) => forward_results(inner, tx, framed).await,
    #[cfg(any(feature = "enable-rustls", feature = "enable-rustls-ring"))]
    ConnectionKind::Rustls(framed) => forward_results(inner, tx, framed).await,
    #[cfg(feature = "enable-native-tls")]
    ConnectionKind::NativeTls(framed) => forward_results(inner, tx, framed).await,
    #[cfg(feature = "unix-sockets")]
    ConnectionKind::Unix(framed) => forward_results(inner, tx, framed).await,
  };

  _warn!(inner, "Stopping monitor stream.");
}

pub async fn start(config: Config) -> Result<impl Stream<Item = MonitorCommand>, Error> {
  let connection = ConnectionConfig::default();
  let server = match config.server {
    ServerConfig::Centralized { ref server } => server.clone(),
    _ => return Err(Error::new(ErrorKind::Config, "Expected centralized server config.")),
  };

  let inner = ClientInner::new(config, PerformanceConfig::default(), connection, None);
  let mut connection = connection::create(&inner, &server, None).await?;
  connection.setup(&inner, None).await?;
  let connection = send_monitor_command(&inner, connection).await?;

  // there isn't really a mechanism to surface backpressure to the server for the MONITOR stream, so we use a
  // background task with a channel to process the frames so that the server can keep sending data even if the
  // stream consumer slows down processing the frames.
  let (tx, rx) = channel(0);
  spawn(async move {
    process_stream(&inner, tx, connection).await;
  });

  Ok(rx.into_stream())
}
