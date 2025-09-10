use super::*;
use crate::{
  error::*,
  interfaces,
  modules::inner::ClientInner,
  protocol::{
    command::{Command, CommandKind},
    responders::ResponseKind,
    types::*,
  },
  runtime::{channel, RefCount},
  types::{scan::*, ClusterHash, Key, Value},
  utils,
};
use bytes_utils::Str;
use futures::stream::{Stream, TryStreamExt};

static STARTING_CURSOR: &str = "0";

fn values_args(key: Key, pattern: Str, count: Option<u32>) -> Vec<Value> {
  let mut args = Vec::with_capacity(6);
  args.push(key.into());
  args.push(static_val!(STARTING_CURSOR));
  args.push(static_val!(MATCH));
  args.push(pattern.into());

  if let Some(count) = count {
    args.push(static_val!(COUNT));
    args.push(count.into());
  }

  args
}

fn create_scan_args(
  args: &mut Vec<Value>,
  pattern: Str,
  count: Option<u32>,
  r#type: Option<ScanType>,
  cursor: Option<Value>,
) {
  args.push(cursor.unwrap_or_else(|| static_val!(STARTING_CURSOR)));
  args.push(static_val!(MATCH));
  args.push(pattern.into());

  if let Some(count) = count {
    args.push(static_val!(COUNT));
    args.push(count.into());
  }
  if let Some(r#type) = r#type {
    args.push(static_val!(TYPE));
    args.push(r#type.to_str().into());
  }
}

fn pattern_hash_slot(inner: &RefCount<ClientInner>, pattern: &str) -> Option<u16> {
  if inner.config.server.is_clustered() {
    if utils::clustered_scan_pattern_has_hash_tag(inner, pattern) {
      Some(redis_protocol::redis_keyslot(pattern.as_bytes()))
    } else {
      None
    }
  } else {
    None
  }
}

pub fn scan_cluster(
  inner: &RefCount<ClientInner>,
  pattern: Str,
  count: Option<u32>,
  r#type: Option<ScanType>,
) -> impl Stream<Item = Result<ScanResult, Error>> {
  let (tx, rx) = channel(0);

  let hash_slots = inner.with_cluster_state(|state| Ok(state.unique_hash_slots()));
  let hash_slots = match hash_slots {
    Ok(slots) => slots,
    Err(e) => {
      let _ = tx.try_send(Err(e));
      return rx.into_stream();
    },
  };

  let mut args = Vec::with_capacity(7);
  create_scan_args(&mut args, pattern, count, r#type, None);
  for slot in hash_slots.into_iter() {
    _trace!(inner, "Scan cluster hash slot server: {}", slot);
    let response = ResponseKind::KeyScan(KeyScanInner {
      hash_slot:  Some(slot),
      args:       args.clone(),
      cursor_idx: 0,
      tx:         tx.clone(),
      server:     None,
    });
    let command: Command = (CommandKind::Scan, Vec::new(), response).into();

    if let Err(e) = interfaces::default_send_command(inner, command) {
      let _ = tx.try_send(Err(e));
      break;
    }
  }

  rx.into_stream()
}

pub fn scan_cluster_buffered(
  inner: &RefCount<ClientInner>,
  pattern: Str,
  count: Option<u32>,
  r#type: Option<ScanType>,
) -> impl Stream<Item = Result<Key, Error>> {
  let (tx, rx) = channel(0);

  let hash_slots = inner.with_cluster_state(|state| Ok(state.unique_hash_slots()));
  let hash_slots = match hash_slots {
    Ok(slots) => slots,
    Err(e) => {
      let _ = tx.try_send(Err(e));
      return rx.into_stream();
    },
  };

  let mut args = Vec::with_capacity(7);
  create_scan_args(&mut args, pattern, count, r#type, None);
  for slot in hash_slots.into_iter() {
    _trace!(inner, "Scan cluster buffered hash slot server: {}", slot);
    let response = ResponseKind::KeyScanBuffered(KeyScanBufferedInner {
      hash_slot:  Some(slot),
      args:       args.clone(),
      cursor_idx: 0,
      tx:         tx.clone(),
      server:     None,
    });
    let command: Command = (CommandKind::Scan, Vec::new(), response).into();

    if let Err(e) = interfaces::default_send_command(inner, command) {
      let _ = tx.try_send(Err(e));
      break;
    }
  }

  rx.into_stream()
}

pub async fn scan_page<C: ClientLike>(
  client: &C,
  cursor: Str,
  pattern: Str,
  count: Option<u32>,
  r#type: Option<ScanType>,
  server: Option<Server>,
  cluster_hash: Option<ClusterHash>,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let hash_slot = pattern_hash_slot(client.inner(), &pattern);
    let mut args = Vec::with_capacity(7);
    create_scan_args(&mut args, pattern, count, r#type, Some(cursor.into()));

    let mut command = Command::new(CommandKind::Scan, args);
    if let Some(server) = server {
      command.cluster_node = Some(server);
    } else if let Some(hasher) = cluster_hash {
      command.hasher = hasher;
    } else if let Some(slot) = hash_slot {
      command.hasher = ClusterHash::Custom(slot);
    }
    Ok(command)
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub fn scan(
  inner: &RefCount<ClientInner>,
  pattern: Str,
  count: Option<u32>,
  r#type: Option<ScanType>,
) -> impl Stream<Item = Result<ScanResult, Error>> {
  let (tx, rx) = channel(0);

  let hash_slot = pattern_hash_slot(inner, &pattern);
  let mut args = Vec::with_capacity(7);
  create_scan_args(&mut args, pattern, count, r#type, None);
  let response = ResponseKind::KeyScan(KeyScanInner {
    hash_slot,
    args,
    server: None,
    cursor_idx: 0,
    tx: tx.clone(),
  });
  let command: Command = (CommandKind::Scan, Vec::new(), response).into();

  if let Err(e) = interfaces::default_send_command(inner, command) {
    let _ = tx.try_send(Err(e));
  }

  rx.into_stream()
}

pub fn scan_buffered(
  inner: &RefCount<ClientInner>,
  pattern: Str,
  count: Option<u32>,
  r#type: Option<ScanType>,
  server: Option<Server>,
) -> impl Stream<Item = Result<Key, Error>> {
  let (tx, rx) = channel(0);

  let hash_slot = pattern_hash_slot(inner, &pattern);
  let mut args = Vec::with_capacity(7);
  create_scan_args(&mut args, pattern, count, r#type, None);
  let response = ResponseKind::KeyScanBuffered(KeyScanBufferedInner {
    hash_slot,
    args,
    server,
    cursor_idx: 0,
    tx: tx.clone(),
  });
  let command: Command = (CommandKind::Scan, Vec::new(), response).into();

  if let Err(e) = interfaces::default_send_command(inner, command) {
    let _ = tx.try_send(Err(e));
  }

  rx.into_stream()
}

pub fn hscan(
  inner: &RefCount<ClientInner>,
  key: Key,
  pattern: Str,
  count: Option<u32>,
) -> impl Stream<Item = Result<HScanResult, Error>> {
  let (tx, rx) = channel(0);
  let args = values_args(key, pattern, count);

  let response = ResponseKind::ValueScan(ValueScanInner {
    tx: tx.clone(),
    cursor_idx: 1,
    args,
  });
  let command: Command = (CommandKind::Hscan, Vec::new(), response).into();
  if let Err(e) = interfaces::default_send_command(inner, command) {
    let _ = tx.try_send(Err(e));
  }

  rx.into_stream().try_filter_map(|result| async move {
    match result {
      ValueScanResult::HScan(res) => Ok(Some(res)),
      _ => Err(Error::new(ErrorKind::Protocol, "Expected HSCAN result.")),
    }
  })
}

pub fn sscan(
  inner: &RefCount<ClientInner>,
  key: Key,
  pattern: Str,
  count: Option<u32>,
) -> impl Stream<Item = Result<SScanResult, Error>> {
  let (tx, rx) = channel(0);
  let args = values_args(key, pattern, count);

  let response = ResponseKind::ValueScan(ValueScanInner {
    tx: tx.clone(),
    cursor_idx: 1,
    args,
  });
  let command: Command = (CommandKind::Sscan, Vec::new(), response).into();

  if let Err(e) = interfaces::default_send_command(inner, command) {
    let _ = tx.try_send(Err(e));
  }

  rx.into_stream().try_filter_map(|result| async move {
    match result {
      ValueScanResult::SScan(res) => Ok(Some(res)),
      _ => Err(Error::new(ErrorKind::Protocol, "Expected SSCAN result.")),
    }
  })
}

pub fn zscan(
  inner: &RefCount<ClientInner>,
  key: Key,
  pattern: Str,
  count: Option<u32>,
) -> impl Stream<Item = Result<ZScanResult, Error>> {
  let inner = inner.clone();
  let (tx, rx) = channel(0);
  let args = values_args(key, pattern, count);

  let response = ResponseKind::ValueScan(ValueScanInner {
    tx: tx.clone(),
    cursor_idx: 1,
    args,
  });
  let command: Command = (CommandKind::Zscan, Vec::new(), response).into();

  if let Err(e) = interfaces::default_send_command(&inner, command) {
    let _ = tx.try_send(Err(e));
  }

  rx.into_stream().try_filter_map(|result| async move {
    match result {
      ValueScanResult::ZScan(res) => Ok(Some(res)),
      _ => Err(Error::new(ErrorKind::Protocol, "Expected ZSCAN result.")),
    }
  })
}
