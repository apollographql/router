use crate::{
  clients::Client,
  interfaces,
  modules::inner::ClientInner,
  protocol::{
    command::{Command, CommandKind},
    responders::ResponseKind,
    types::{KeyScanInner, ValueScanInner},
  },
  runtime::RefCount,
  types::{Key, Map, Value},
  utils,
};
use bytes_utils::Str;
use std::borrow::Cow;

/// The types of values supported by the [type](https://redis.io/commands/type) command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScanType {
  Set,
  String,
  ZSet,
  List,
  Hash,
  Stream,
}

impl ScanType {
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      ScanType::Set => "set",
      ScanType::String => "string",
      ScanType::List => "list",
      ScanType::ZSet => "zset",
      ScanType::Hash => "hash",
      ScanType::Stream => "stream",
    })
  }
}

/// An interface for interacting with the results of a scan operation.
pub trait Scanner {
  /// The type of results from the scan operation.
  type Page;

  /// Read the cursor returned from the last scan operation.
  fn cursor(&self) -> Option<Cow<str>>;

  /// Whether the scan call will continue returning results. If `false` this will be the last result set
  /// returned on the stream.
  ///
  /// Calling `next` when this returns `false` will return `Ok(())`, so this does not need to be checked on each
  /// result.
  fn has_more(&self) -> bool;

  /// Return a reference to the last page of results.
  fn results(&self) -> &Option<Self::Page>;

  /// Take ownership over the results of the SCAN operation. Calls to `results` or `take_results` will return `None`
  /// afterwards.
  fn take_results(&mut self) -> Option<Self::Page>;

  /// A lightweight function to create a client from the SCAN result.
  ///
  /// To continue scanning the caller should call `next` on this struct. Calling `scan` again on the client will
  /// initiate a new SCAN call starting with a cursor of 0.
  fn create_client(&self) -> Client;

  /// Move on to the next page of results from the SCAN operation. If no more results are available this may close the
  /// stream. This interface provides a mechanism for throttling the throughput of the SCAN call
  ///
  /// If callers do not call this function the scanning will continue when this struct is dropped. Results are not
  /// automatically scanned in the background since this could cause the buffer backing the stream to grow too large
  /// very quickly. Callers can use [scan_buffered](crate::clients::Client::scan_buffered) or
  /// [scan_cluster_buffered](crate::clients::Client::scan_cluster_buffered) to automatically continue scanning
  /// in the background.
  fn next(self);

  /// Stop the scanning process, ending the outer stream.
  fn cancel(self);
}

/// The result of a SCAN operation.
pub struct ScanResult {
  pub(crate) results:      Option<Vec<Key>>,
  pub(crate) inner:        RefCount<ClientInner>,
  pub(crate) scan_state:   Option<KeyScanInner>,
  pub(crate) can_continue: bool,
}

fn next_key_page(inner: &RefCount<ClientInner>, state: &mut Option<KeyScanInner>) {
  if let Some(state) = state.take() {
    let cluster_node = state.server.clone();
    let response = ResponseKind::KeyScan(state);
    let mut cmd: Command = (CommandKind::Scan, Vec::new(), response).into();
    cmd.cluster_node = cluster_node;

    let _ = interfaces::default_send_command(inner, cmd);
  }
}

impl Drop for ScanResult {
  fn drop(&mut self) {
    if self.can_continue {
      next_key_page(&self.inner, &mut self.scan_state);
    }
  }
}

impl Scanner for ScanResult {
  type Page = Vec<Key>;

  fn cursor(&self) -> Option<Cow<str>> {
    if let Some(ref state) = self.scan_state {
      state.args[state.cursor_idx].as_str()
    } else {
      None
    }
  }

  fn has_more(&self) -> bool {
    self.can_continue
  }

  fn results(&self) -> &Option<Self::Page> {
    &self.results
  }

  fn take_results(&mut self) -> Option<Self::Page> {
    self.results.take()
  }

  fn create_client(&self) -> Client {
    Client {
      inner: self.inner.clone(),
    }
  }

  fn next(self) {
    if !self.can_continue {
      return;
    }

    let mut _self = self;
    next_key_page(&_self.inner, &mut _self.scan_state);
  }

  fn cancel(mut self) {
    let _ = self.scan_state.take();
  }
}

/// The result of a HSCAN operation.
pub struct HScanResult {
  pub(crate) results:      Option<Map>,
  pub(crate) inner:        RefCount<ClientInner>,
  pub(crate) scan_state:   Option<ValueScanInner>,
  pub(crate) can_continue: bool,
}

fn next_hscan_page(inner: &RefCount<ClientInner>, state: &mut Option<ValueScanInner>) {
  if let Some(state) = state.take() {
    let response = ResponseKind::ValueScan(state);
    let cmd: Command = (CommandKind::Hscan, Vec::new(), response).into();
    let _ = interfaces::default_send_command(inner, cmd);
  }
}

impl Drop for HScanResult {
  fn drop(&mut self) {
    if self.can_continue {
      next_hscan_page(&self.inner, &mut self.scan_state);
    }
  }
}

impl Scanner for HScanResult {
  type Page = Map;

  fn cursor(&self) -> Option<Cow<str>> {
    if let Some(ref state) = self.scan_state {
      state.args[state.cursor_idx].as_str()
    } else {
      None
    }
  }

  fn has_more(&self) -> bool {
    self.can_continue
  }

  fn results(&self) -> &Option<Self::Page> {
    &self.results
  }

  fn take_results(&mut self) -> Option<Self::Page> {
    self.results.take()
  }

  fn create_client(&self) -> Client {
    Client {
      inner: self.inner.clone(),
    }
  }

  fn next(self) {
    if !self.can_continue {
      return;
    }

    let mut _self = self;
    next_hscan_page(&_self.inner, &mut _self.scan_state);
  }

  fn cancel(mut self) {
    let _ = self.scan_state.take();
  }
}

/// The result of a SSCAN operation.
pub struct SScanResult {
  pub(crate) results:      Option<Vec<Value>>,
  pub(crate) inner:        RefCount<ClientInner>,
  pub(crate) scan_state:   Option<ValueScanInner>,
  pub(crate) can_continue: bool,
}

fn next_sscan_page(inner: &RefCount<ClientInner>, state: &mut Option<ValueScanInner>) {
  if let Some(state) = state.take() {
    let response = ResponseKind::ValueScan(state);
    let cmd: Command = (CommandKind::Sscan, Vec::new(), response).into();
    let _ = interfaces::default_send_command(inner, cmd);
  }
}

impl Drop for SScanResult {
  fn drop(&mut self) {
    if self.can_continue {
      next_sscan_page(&self.inner, &mut self.scan_state);
    }
  }
}

impl Scanner for SScanResult {
  type Page = Vec<Value>;

  fn cursor(&self) -> Option<Cow<str>> {
    if let Some(ref state) = self.scan_state {
      state.args[state.cursor_idx].as_str()
    } else {
      None
    }
  }

  fn results(&self) -> &Option<Self::Page> {
    &self.results
  }

  fn take_results(&mut self) -> Option<Self::Page> {
    self.results.take()
  }

  fn has_more(&self) -> bool {
    self.can_continue
  }

  fn create_client(&self) -> Client {
    Client {
      inner: self.inner.clone(),
    }
  }

  fn next(self) {
    if !self.can_continue {
      return;
    }

    let mut _self = self;
    next_sscan_page(&_self.inner, &mut _self.scan_state);
  }

  fn cancel(mut self) {
    let _ = self.scan_state.take();
  }
}

/// The result of a ZSCAN operation.
pub struct ZScanResult {
  pub(crate) results:      Option<Vec<(Value, f64)>>,
  pub(crate) inner:        RefCount<ClientInner>,
  pub(crate) scan_state:   Option<ValueScanInner>,
  pub(crate) can_continue: bool,
}

fn next_zscan_page(inner: &RefCount<ClientInner>, state: &mut Option<ValueScanInner>) {
  if let Some(state) = state.take() {
    let response = ResponseKind::ValueScan(state);
    let cmd: Command = (CommandKind::Zscan, Vec::new(), response).into();
    let _ = interfaces::default_send_command(inner, cmd);
  }
}

impl Drop for ZScanResult {
  fn drop(&mut self) {
    if self.can_continue {
      next_zscan_page(&self.inner, &mut self.scan_state);
    }
  }
}

impl Scanner for ZScanResult {
  type Page = Vec<(Value, f64)>;

  fn cursor(&self) -> Option<Cow<str>> {
    if let Some(ref state) = self.scan_state {
      state.args[state.cursor_idx].as_str()
    } else {
      None
    }
  }

  fn has_more(&self) -> bool {
    self.can_continue
  }

  fn results(&self) -> &Option<Self::Page> {
    &self.results
  }

  fn take_results(&mut self) -> Option<Self::Page> {
    self.results.take()
  }

  fn create_client(&self) -> Client {
    Client {
      inner: self.inner.clone(),
    }
  }

  fn next(self) {
    if !self.can_continue {
      return;
    }

    let mut _self = self;
    next_zscan_page(&_self.inner, &mut _self.scan_state);
  }

  fn cancel(mut self) {
    let _ = self.scan_state.take();
  }
}
