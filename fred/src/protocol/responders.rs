use crate::{
  error::{Error, ErrorKind},
  interfaces,
  interfaces::Resp3Frame,
  modules::inner::ClientInner,
  protocol::{
    command::{Command, CommandKind, ResponseSender},
    types::{KeyScanBufferedInner, KeyScanInner, Server, ValueScanInner, ValueScanResult},
    utils as protocol_utils,
  },
  runtime::{AtomicUsize, Mutex, RefCount},
  types::{
    scan::{HScanResult, SScanResult, ScanResult, ZScanResult},
    Key,
    Value,
  },
  utils as client_utils,
};
use bytes_utils::Str;
use redis_protocol::resp3::types::{FrameKind, Resp3Frame as _Resp3Frame};
use std::{fmt, fmt::Formatter, mem, ops::DerefMut};

#[cfg(feature = "metrics")]
use crate::modules::metrics::MovingStats;
#[cfg(feature = "metrics")]
use crate::runtime::RwLock;
#[cfg(feature = "metrics")]
use std::{cmp, time::Instant};

const LAST_CURSOR: &str = "0";

pub enum ResponseKind {
  /// Throw away the response frame and last command in the command buffer.
  ///
  /// Equivalent to `Respond(None)`.
  Skip,
  /// Respond to the caller of the last command with the response frame.
  Respond(Option<ResponseSender>),
  /// Buffer multiple response frames until the expected number of frames are received, then respond with an array to
  /// the caller.
  ///
  /// Typically used in `*_cluster` commands or to handle concurrent responses in a `Pipeline` that may span multiple
  /// cluster connections.
  Buffer {
    /// A shared buffer for response frames.
    frames:      RefCount<Mutex<Vec<Resp3Frame>>>,
    /// The expected number of response frames.
    expected:    usize,
    /// The number of response frames received.
    received:    RefCount<AtomicUsize>,
    /// A shared oneshot channel to the caller.
    tx:          RefCount<Mutex<Option<ResponseSender>>>,
    /// A local field for tracking the expected index of the response in the `frames` array.
    index:       usize,
    /// Whether errors should be returned early to the caller.
    error_early: bool,
  },
  /// Handle the response as a page of key/value pairs from a HSCAN, SSCAN, ZSCAN command.
  ValueScan(ValueScanInner),
  /// Handle the response as a page of keys from a SCAN command.
  KeyScan(KeyScanInner),
  /// Handle the response as a buffered key SCAN command.
  KeyScanBuffered(KeyScanBufferedInner),
}

impl fmt::Debug for ResponseKind {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    write!(f, "{}", match self {
      ResponseKind::Skip => "Skip",
      ResponseKind::Buffer { .. } => "Buffer",
      ResponseKind::Respond(_) => "Respond",
      ResponseKind::KeyScan(_) => "KeyScan",
      ResponseKind::ValueScan(_) => "ValueScan",
      ResponseKind::KeyScanBuffered(_) => "KeyScanBuffered",
    })
  }
}

impl ResponseKind {
  /// Attempt to clone the response channel.
  ///
  /// If the channel cannot be shared or cloned (since it contains a oneshot channel) this will fall back to a `Skip`
  /// policy.
  pub fn duplicate(&self) -> Option<Self> {
    Some(match self {
      ResponseKind::Skip => ResponseKind::Skip,
      ResponseKind::Respond(_) => ResponseKind::Respond(None),
      ResponseKind::Buffer {
        frames,
        tx,
        received,
        index,
        expected,
        error_early,
      } => ResponseKind::Buffer {
        frames:      frames.clone(),
        tx:          tx.clone(),
        received:    received.clone(),
        index:       *index,
        expected:    *expected,
        error_early: *error_early,
      },
      ResponseKind::KeyScan(_) | ResponseKind::ValueScan(_) | ResponseKind::KeyScanBuffered(_) => return None,
    })
  }

  pub fn set_expected_index(&mut self, idx: usize) {
    if let ResponseKind::Buffer { ref mut index, .. } = self {
      *index = idx;
    }
  }

  pub fn set_error_early(&mut self, _error_early: bool) {
    if let ResponseKind::Buffer {
      ref mut error_early, ..
    } = self
    {
      *error_early = _error_early;
    }
  }

  pub fn new_buffer(tx: ResponseSender) -> Self {
    ResponseKind::Buffer {
      frames:      RefCount::new(Mutex::new(vec![])),
      tx:          RefCount::new(Mutex::new(Some(tx))),
      received:    RefCount::new(AtomicUsize::new(0)),
      index:       0,
      expected:    0,
      error_early: true,
    }
  }

  pub fn new_buffer_with_size(expected: usize, tx: ResponseSender) -> Self {
    ResponseKind::Buffer {
      frames: RefCount::new(Mutex::new(vec![Resp3Frame::Null; expected])),
      tx: RefCount::new(Mutex::new(Some(tx))),
      received: RefCount::new(AtomicUsize::new(0)),
      index: 0,
      error_early: true,
      expected,
    }
  }

  /// Take the oneshot response sender.
  pub fn take_response_tx(&mut self) -> Option<ResponseSender> {
    match self {
      ResponseKind::Respond(tx) => tx.take(),
      ResponseKind::Buffer { tx, .. } => tx.lock().take(),
      _ => None,
    }
  }

  /// Clone the shared response sender for `Buffer` or `Multiple` variants.
  pub fn clone_shared_response_tx(&self) -> Option<RefCount<Mutex<Option<ResponseSender>>>> {
    match self {
      ResponseKind::Buffer { tx, .. } => Some(tx.clone()),
      _ => None,
    }
  }

  /// Respond with an error to the caller.
  pub fn respond_with_error(&mut self, error: Error) {
    if let Some(tx) = self.take_response_tx() {
      let _ = tx.send(Err(error));
    }
  }

  /// Read the number of expected response frames.
  pub fn expected_response_frames(&self) -> usize {
    match self {
      ResponseKind::Skip | ResponseKind::Respond(_) => 1,
      ResponseKind::Buffer { ref expected, .. } => *expected,
      ResponseKind::ValueScan(_) | ResponseKind::KeyScan(_) | ResponseKind::KeyScanBuffered(_) => 1,
    }
  }

  /// Whether the responder is a `ResponseKind::Buffer`.
  pub fn is_buffer(&self) -> bool {
    matches!(self, ResponseKind::Buffer { .. })
  }
}

#[cfg(feature = "metrics")]
fn sample_latency(latency_stats: &RwLock<MovingStats>, sent: Instant) {
  let dur = Instant::now().duration_since(sent);
  let dur_ms = cmp::max(0, (dur.as_secs() * 1000) + dur.subsec_millis() as u64) as i64;
  latency_stats.write().sample(dur_ms);
}

/// Sample overall and network latency values for a command.
#[cfg(feature = "metrics")]
pub fn sample_command_latencies(inner: &RefCount<ClientInner>, command: &mut Command) {
  if let Some(sent) = command.network_start.take() {
    sample_latency(&inner.network_latency_stats, sent);
  }
  sample_latency(&inner.latency_stats, command.created);
}

#[cfg(not(feature = "metrics"))]
pub fn sample_command_latencies(_: &RefCount<ClientInner>, _: &mut Command) {}

/// Update the client's protocol version codec version after receiving a non-error response to HELLO.
fn update_protocol_version(inner: &RefCount<ClientInner>, command: &Command, frame: &Resp3Frame) {
  if !matches!(frame.kind(), FrameKind::SimpleError | FrameKind::BlobError) {
    let version = match command.kind {
      CommandKind::_Hello(ref version) => version,
      CommandKind::_HelloAllCluster(ref version) => version,
      _ => return,
    };

    _debug!(inner, "Changing RESP version to {:?}", version);
    // HELLO is not pipelined so this is safe
    inner.switch_protocol_versions(version.clone());
  }
}

fn respond_locked(
  inner: &RefCount<ClientInner>,
  tx: &RefCount<Mutex<Option<ResponseSender>>>,
  result: Result<Resp3Frame, Error>,
) {
  if let Some(tx) = tx.lock().take() {
    if let Err(_) = tx.send(result) {
      _debug!(inner, "Error responding to caller.");
    }
  }
}

/// Add the provided frame to the response buffer.
fn buffer_frame(
  server: &Server,
  buffer: &RefCount<Mutex<Vec<Resp3Frame>>>,
  index: usize,
  frame: Resp3Frame,
) -> Result<(), Error> {
  let mut guard = buffer.lock();
  let buffer_ref = guard.deref_mut();

  if index >= buffer_ref.len() {
    return Err(Error::new(ErrorKind::Unknown, "Invalid buffer response index."));
  }

  trace!(
    "({}) Add buffered frame {:?} at index {} with length {}",
    server,
    frame.kind(),
    index,
    buffer_ref.len()
  );
  buffer_ref[index] = frame;
  Ok(())
}

/// Check for errors while merging the provided frames into one Array frame.
fn merge_multiple_frames(frames: &mut Vec<Resp3Frame>, error_early: bool) -> Resp3Frame {
  if error_early {
    for frame in frames.iter() {
      if matches!(frame.kind(), FrameKind::SimpleError | FrameKind::BlobError) {
        return frame.clone();
      }
    }
  }

  Resp3Frame::Array {
    data:       mem::take(frames),
    attributes: None,
  }
}

/// Parse the output of a command that scans keys.
fn parse_key_scan_frame(frame: Resp3Frame) -> Result<(Str, Vec<Key>), Error> {
  if let Resp3Frame::Array { mut data, .. } = frame {
    if data.len() == 2 {
      let cursor = match protocol_utils::frame_to_str(data[0].clone()) {
        Some(s) => s,
        None => {
          return Err(Error::new(
            ErrorKind::Protocol,
            "Expected first SCAN result element to be a bulk string.",
          ))
        },
      };

      if let Some(Resp3Frame::Array { data, .. }) = data.pop() {
        let mut keys = Vec::with_capacity(data.len());

        for frame in data.into_iter() {
          let key = match protocol_utils::frame_to_bytes(frame) {
            Some(s) => s,
            None => {
              return Err(Error::new(
                ErrorKind::Protocol,
                "Expected an array of strings from second SCAN result.",
              ))
            },
          };

          keys.push(key.into());
        }

        Ok((cursor, keys))
      } else {
        Err(Error::new(
          ErrorKind::Protocol,
          "Expected second SCAN result element to be an array.",
        ))
      }
    } else {
      Err(Error::new(
        ErrorKind::Protocol,
        "Expected two-element bulk string array from SCAN.",
      ))
    }
  } else {
    Err(Error::new(ErrorKind::Protocol, "Expected bulk string array from SCAN."))
  }
}

/// Parse the output of a command that scans values.
fn parse_value_scan_frame(frame: Resp3Frame) -> Result<(Str, Vec<Value>), Error> {
  if let Resp3Frame::Array { mut data, .. } = frame {
    if data.len() == 2 {
      let cursor = match protocol_utils::frame_to_str(data[0].clone()) {
        Some(s) => s,
        None => {
          return Err(Error::new(
            ErrorKind::Protocol,
            "Expected first result element to be a bulk string.",
          ))
        },
      };

      if let Some(Resp3Frame::Array { data, .. }) = data.pop() {
        let mut values = Vec::with_capacity(data.len());

        for frame in data.into_iter() {
          values.push(protocol_utils::frame_to_results(frame)?);
        }

        Ok((cursor, values))
      } else {
        Err(Error::new(
          ErrorKind::Protocol,
          "Expected second result element to be an array.",
        ))
      }
    } else {
      Err(Error::new(
        ErrorKind::Protocol,
        "Expected two-element bulk string array.",
      ))
    }
  } else {
    Err(Error::new(ErrorKind::Protocol, "Expected bulk string array."))
  }
}

/// Send the output to the caller of a command that scans values.
fn send_value_scan_result(
  inner: &RefCount<ClientInner>,
  scanner: ValueScanInner,
  command: &Command,
  result: Vec<Value>,
  can_continue: bool,
) -> Result<(), Error> {
  match command.kind {
    CommandKind::Zscan => {
      let tx = scanner.tx.clone();
      let results = ValueScanInner::transform_zscan_result(result)?;

      let state = ValueScanResult::ZScan(ZScanResult {
        can_continue,
        inner: inner.clone(),
        scan_state: Some(scanner),
        results: Some(results),
      });

      if let Err(_) = tx.try_send(Ok(state)) {
        _warn!(inner, "Failed to send ZSCAN result to caller");
      }
    },
    CommandKind::Sscan => {
      let tx = scanner.tx.clone();

      let state = ValueScanResult::SScan(SScanResult {
        can_continue,
        inner: inner.clone(),
        scan_state: Some(scanner),
        results: Some(result),
      });

      if let Err(_) = tx.try_send(Ok(state)) {
        _warn!(inner, "Failed to send SSCAN result to caller");
      }
    },
    CommandKind::Hscan => {
      let tx = scanner.tx.clone();
      let results = ValueScanInner::transform_hscan_result(result)?;

      let state = ValueScanResult::HScan(HScanResult {
        can_continue,
        inner: inner.clone(),
        scan_state: Some(scanner),
        results: Some(results),
      });

      if let Err(_) = tx.try_send(Ok(state)) {
        _warn!(inner, "Failed to send HSCAN result to caller");
      }
    },
    _ => {
      return Err(Error::new(
        ErrorKind::Unknown,
        "Invalid redis command. Expected HSCAN, SSCAN, or ZSCAN.",
      ))
    },
  };

  Ok(())
}

/// Respond to the caller with the default response policy.
pub fn respond_to_caller(
  inner: &RefCount<ClientInner>,
  server: &Server,
  mut command: Command,
  tx: ResponseSender,
  frame: Resp3Frame,
) -> Result<(), Error> {
  sample_command_latencies(inner, &mut command);
  _trace!(
    inner,
    "Respond to caller from {} for {} with {:?}",
    server,
    command.kind.to_str_debug(),
    frame.kind()
  );
  if command.kind.is_hello() {
    update_protocol_version(inner, &command, &frame);
  }

  let _ = tx.send(Ok(frame));
  Ok(())
}

/// Respond to the caller, assuming multiple response frames from the last command, storing intermediate responses in
/// the shared buffer.
pub fn respond_buffer(
  inner: &RefCount<ClientInner>,
  server: &Server,
  command: Command,
  received: RefCount<AtomicUsize>,
  expected: usize,
  error_early: bool,
  frames: RefCount<Mutex<Vec<Resp3Frame>>>,
  index: usize,
  tx: RefCount<Mutex<Option<ResponseSender>>>,
  frame: Resp3Frame,
) -> Result<(), Error> {
  _trace!(
    inner,
    "Handling `buffer` response from {} for {}. kind {:?}, Index: {}, ID: {}",
    server,
    command.kind.to_str_debug(),
    frame.kind(),
    index,
    command.debug_id()
  );
  let closes_connection = command.kind.closes_connection();

  // errors are buffered like normal frames and are not returned early
  if let Err(e) = buffer_frame(server, &frames, index, frame) {
    if closes_connection {
      _debug!(inner, "Ignoring unexpected buffer response index from QUIT or SHUTDOWN");
      respond_locked(inner, &tx, Err(Error::new_canceled()));
      return Err(Error::new_canceled());
    } else {
      respond_locked(inner, &tx, Err(e));
      _error!(
        inner,
        "Exiting early after unexpected buffer response index from {} with command {}, ID {}",
        server,
        command.kind.to_str_debug(),
        command.debug_id()
      );
      return Err(Error::new(ErrorKind::Unknown, "Invalid buffer response index."));
    }
  }

  let received = client_utils::incr_atomic(&received);
  if received == expected {
    _trace!(
      inner,
      "Responding to caller after last buffered response from {}, ID: {}",
      server,
      command.debug_id()
    );

    let frame = merge_multiple_frames(&mut frames.lock(), error_early);
    if matches!(frame.kind(), FrameKind::SimpleError | FrameKind::BlobError) {
      let err = match frame.as_str() {
        Some(s) => protocol_utils::pretty_error(s),
        None => Error::new(ErrorKind::Unknown, "Unknown or invalid error from buffered frames."),
      };

      respond_locked(inner, &tx, Err(err));
    } else {
      respond_locked(inner, &tx, Ok(frame));
    }
  } else {
    _trace!(
      inner,
      "({}) Waiting on {} more responses",
      command.debug_id(),
      expected - received,
    );
    // this response type is shared across connections so we do not return the command to be re-queued
  }

  Ok(())
}

/// Respond to the caller of a key scanning operation.
pub fn respond_key_scan(
  inner: &RefCount<ClientInner>,
  server: &Server,
  command: Command,
  mut scanner: KeyScanInner,
  frame: Resp3Frame,
) -> Result<(), Error> {
  _trace!(
    inner,
    "Handling `KeyScan` response from {} for {}",
    server,
    command.kind.to_str_debug()
  );
  let (next_cursor, keys) = match parse_key_scan_frame(frame) {
    Ok(result) => result,
    Err(e) => {
      scanner.send_error(e);
      return Ok(());
    },
  };
  let scan_stream = scanner.tx.clone();
  let can_continue = next_cursor != LAST_CURSOR;
  scanner.update_cursor(next_cursor);

  let scan_result = ScanResult {
    scan_state: Some(scanner),
    inner: inner.clone(),
    results: Some(keys),
    can_continue,
  };
  if let Err(_) = scan_stream.try_send(Ok(scan_result)) {
    _debug!(inner, "Error sending SCAN page.");
  }

  Ok(())
}

pub fn respond_key_scan_buffered(
  inner: &RefCount<ClientInner>,
  server: &Server,
  command: Command,
  mut scanner: KeyScanBufferedInner,
  frame: Resp3Frame,
) -> Result<(), Error> {
  _trace!(
    inner,
    "Handling `KeyScanBuffered` response from {} for {}",
    server,
    command.kind.to_str_debug()
  );

  let (next_cursor, keys) = match parse_key_scan_frame(frame) {
    Ok(result) => result,
    Err(e) => {
      scanner.send_error(e);
      return Ok(());
    },
  };
  let scan_stream = scanner.tx.clone();
  let can_continue = next_cursor != LAST_CURSOR;
  scanner.update_cursor(next_cursor);

  for key in keys.into_iter() {
    if let Err(_) = scan_stream.try_send(Ok(key)) {
      _debug!(inner, "Error sending SCAN key.");
      break;
    }
  }
  if can_continue {
    let mut command = Command::new(CommandKind::Scan, Vec::new());
    command.response = ResponseKind::KeyScanBuffered(scanner);
    if let Err(e) = interfaces::default_send_command(inner, command) {
      let _ = scan_stream.try_send(Err(e));
    };
  }

  Ok(())
}

/// Respond to the caller of a value scanning operation.
pub fn respond_value_scan(
  inner: &RefCount<ClientInner>,
  server: &Server,
  command: Command,
  mut scanner: ValueScanInner,
  frame: Resp3Frame,
) -> Result<(), Error> {
  _trace!(
    inner,
    "Handling `ValueScan` response from {} for {}",
    server,
    command.kind.to_str_debug()
  );

  let (next_cursor, values) = match parse_value_scan_frame(frame) {
    Ok(result) => result,
    Err(e) => {
      scanner.send_error(e);
      return Ok(());
    },
  };
  let scan_stream = scanner.tx.clone();
  let can_continue = next_cursor != LAST_CURSOR;
  scanner.update_cursor(next_cursor);

  _trace!(inner, "Sending value scan result with {} values", values.len());
  if let Err(e) = send_value_scan_result(inner, scanner, &command, values, can_continue) {
    if let Err(_) = scan_stream.try_send(Err(e)) {
      _warn!(inner, "Error sending scan result.");
    }
  }

  Ok(())
}
