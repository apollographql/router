use crate::{modules::inner::ClientInner, protocol::command::Command, runtime::RefCount};
use redis_protocol::resp3::types::{BytesFrame as Resp3Frame, Resp3Frame as _Resp3Frame};
use std::{fmt, ops::Deref};
pub use tracing::span::Span;
use tracing::{event, field::Empty, Id as TraceId, Level};

#[cfg(not(feature = "full-tracing"))]
use crate::trace::disabled::Span as FakeSpan;

/// Struct for storing spans used by the client when sending a command.
pub struct CommandTraces {
  pub cmd:     Option<Span>,
  pub network: Option<Span>,
  #[cfg(feature = "full-tracing")]
  pub queued:  Option<Span>,
  #[cfg(not(feature = "full-tracing"))]
  pub queued:  Option<FakeSpan>,
}

/// Enter the network span when the command is dropped after receiving a response.
impl Drop for CommandTraces {
  fn drop(&mut self) {
    if let Some(span) = self.network.take() {
      let _enter = span.enter();
    }
  }
}

impl Default for CommandTraces {
  fn default() -> Self {
    CommandTraces {
      cmd:     None,
      queued:  None,
      network: None,
    }
  }
}

impl fmt::Debug for CommandTraces {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f, "[Command Traces]")
  }
}

pub fn set_network_span(inner: &RefCount<ClientInner>, command: &mut Command, flush: bool) {
  trace!("Setting network span from command {}", command.debug_id());
  let span = fspan!(command, inner.tracing_span_level(), "fred.rtt", "cmd.flush" = flush);
  span.in_scope(|| {});
  command.traces.network = Some(span);
}

pub fn record_response_size(span: &Span, frame: &Resp3Frame) {
  #[allow(clippy::needless_borrows_for_generic_args)]
  span.record("cmd.res", &frame.encode_len(true));
}

pub fn create_command_span(inner: &RefCount<ClientInner>) -> Span {
  span_lvl!(
    inner.tracing_span_level(),
    "fred.command",
    module = "fred",
    "client.id" = &inner.id.deref(),
    "cmd.name" = Empty,
    "cmd.req" = Empty,
    "cmd.res" = Empty
  )
}

#[cfg(feature = "full-tracing")]
pub fn create_args_span(parent: Option<TraceId>, inner: &RefCount<ClientInner>) -> Span {
  span_lvl!(inner.full_tracing_span_level(), parent: parent, "fred.prepare", "cmd.args" = Empty)
}

#[cfg(not(feature = "full-tracing"))]
pub fn create_args_span(_parent: Option<TraceId>, _inner: &RefCount<ClientInner>) -> FakeSpan {
  FakeSpan {}
}

#[cfg(feature = "full-tracing")]
pub fn create_queued_span(parent: Option<TraceId>, inner: &RefCount<ClientInner>) -> Span {
  let buf_len = inner.counters.read_cmd_buffer_len();
  span_lvl!(inner.full_tracing_span_level(), parent: parent, "fred.queued", buf_len)
}

#[cfg(not(feature = "full-tracing"))]
pub fn create_queued_span(_parent: Option<TraceId>, _inner: &RefCount<ClientInner>) -> FakeSpan {
  FakeSpan {}
}

#[cfg(feature = "full-tracing")]
pub fn create_pubsub_span(inner: &RefCount<ClientInner>, frame: &Resp3Frame) -> Option<Span> {
  if inner.should_trace() {
    let span = span_lvl!(
      inner.full_tracing_span_level(),
      parent: None,
      "fred.pubsub",
      module = "fred",
      "client.id" = &inner.id.deref(),
      "cmd.res" = &frame.encode_len(true),
      "msg.channel" = Empty
    );

    Some(span)
  } else {
    None
  }
}

#[cfg(not(feature = "full-tracing"))]
pub fn create_pubsub_span(_inner: &RefCount<ClientInner>, _frame: &Resp3Frame) -> Option<FakeSpan> {
  Some(FakeSpan {})
}

pub fn backpressure_event(cmd: &Command, duration: Option<u128>) {
  let id = cmd.traces.cmd.as_ref().and_then(|c| c.id());
  if let Some(duration) = duration {
    event!(parent: id, Level::INFO, "fred.backpressure duration={}", duration);
  } else {
    event!(parent: id, Level::INFO, "fred.backpressure drain");
  }
}
