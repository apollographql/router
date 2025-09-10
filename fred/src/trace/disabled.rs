#![allow(dead_code)]

#[cfg(not(any(feature = "full-tracing", feature = "partial-tracing")))]
use crate::modules::inner::ClientInner;
#[cfg(not(any(feature = "full-tracing", feature = "partial-tracing")))]
use crate::protocol::command::Command;
#[cfg(not(any(feature = "full-tracing", feature = "partial-tracing")))]
use crate::runtime::RefCount;
#[cfg(not(any(feature = "full-tracing", feature = "partial-tracing")))]
use redis_protocol::resp3::types::BytesFrame as Frame;

/// Fake span for mocking tracing functions.
#[cfg(not(feature = "full-tracing"))]
pub struct Span {}

#[cfg(not(feature = "full-tracing"))]
impl Span {
  pub fn enter(&self) {}

  pub fn record<Q: ?Sized, V: ?Sized>(&self, _field: &Q, _value: &V) -> &Self {
    self
  }
}

#[cfg(not(any(feature = "full-tracing", feature = "partial-tracing")))]
pub fn set_network_span(_inner: &RefCount<ClientInner>, _command: &mut Command, _flush: bool) {}

#[cfg(not(any(feature = "full-tracing", feature = "partial-tracing")))]
pub fn create_pubsub_span(_inner: &RefCount<ClientInner>, _frame: &Frame) -> Option<Span> {
  Some(Span {})
}

#[cfg(not(any(feature = "full-tracing", feature = "partial-tracing")))]
pub fn backpressure_event(_cmd: &Command, _: Option<u128>) {}
