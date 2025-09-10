pub use crate::protocol::types::{ClusterRouting, SlotRange};
use crate::{
  error::{Error, ErrorKind},
  types::Value,
  utils,
};
use bytes_utils::Str;

macro_rules! parse_or_zero(
  ($data:ident, $t:ty) => {
    $data.parse::<$t>().ok().unwrap_or(0)
  }
);

fn parse_cluster_info_line(info: &mut ClusterInfo, line: &str) -> Result<(), Error> {
  let parts: Vec<&str> = line.split(':').collect();
  if parts.len() != 2 {
    return Err(Error::new(ErrorKind::Protocol, "Expected key:value pair."));
  }
  let (field, val) = (parts[0], parts[1]);

  match field {
    "cluster_state" => match val {
      "ok" => info.cluster_state = ClusterState::Ok,
      "fail" => info.cluster_state = ClusterState::Fail,
      _ => return Err(Error::new(ErrorKind::Protocol, "Invalid cluster state.")),
    },
    "cluster_slots_assigned" => info.cluster_slots_assigned = parse_or_zero!(val, u16),
    "cluster_slots_ok" => info.cluster_slots_ok = parse_or_zero!(val, u16),
    "cluster_slots_pfail" => info.cluster_slots_pfail = parse_or_zero!(val, u16),
    "cluster_slots_fail" => info.cluster_slots_fail = parse_or_zero!(val, u16),
    "cluster_known_nodes" => info.cluster_known_nodes = parse_or_zero!(val, u16),
    "cluster_size" => info.cluster_size = parse_or_zero!(val, u32),
    "cluster_current_epoch" => info.cluster_current_epoch = parse_or_zero!(val, u64),
    "cluster_my_epoch" => info.cluster_my_epoch = parse_or_zero!(val, u64),
    "cluster_stats_messages_sent" => info.cluster_stats_messages_sent = parse_or_zero!(val, u64),
    "cluster_stats_messages_received" => info.cluster_stats_messages_received = parse_or_zero!(val, u64),
    _ => {
      warn!("Invalid cluster info field: {}", line);
    },
  };

  Ok(())
}

/// The state of the cluster from the `CLUSTER INFO` command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClusterState {
  Ok,
  Fail,
}

impl Default for ClusterState {
  fn default() -> Self {
    ClusterState::Ok
  }
}

/// A parsed response from the `CLUSTER INFO` command.
///
/// <https://redis.io/commands/cluster-info>
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct ClusterInfo {
  pub cluster_state:                   ClusterState,
  pub cluster_slots_assigned:          u16,
  pub cluster_slots_ok:                u16,
  pub cluster_slots_pfail:             u16,
  pub cluster_slots_fail:              u16,
  pub cluster_known_nodes:             u16,
  pub cluster_size:                    u32,
  pub cluster_current_epoch:           u64,
  pub cluster_my_epoch:                u64,
  pub cluster_stats_messages_sent:     u64,
  pub cluster_stats_messages_received: u64,
}

impl TryFrom<Value> for ClusterInfo {
  type Error = Error;

  fn try_from(value: Value) -> Result<Self, Self::Error> {
    if let Some(data) = value.as_bytes_str() {
      let mut out = ClusterInfo::default();

      for line in data.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
          parse_cluster_info_line(&mut out, trimmed)?;
        }
      }
      Ok(out)
    } else {
      Err(Error::new(ErrorKind::Protocol, "Expected string response."))
    }
  }
}

/// Options for the CLUSTER FAILOVER command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClusterFailoverFlag {
  Force,
  Takeover,
}

impl ClusterFailoverFlag {
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      ClusterFailoverFlag::Force => "FORCE",
      ClusterFailoverFlag::Takeover => "TAKEOVER",
    })
  }
}

/// Flags for the CLUSTER RESET command.
///
/// <https://redis.io/commands/cluster-reset>
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClusterResetFlag {
  Hard,
  Soft,
}

impl ClusterResetFlag {
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      ClusterResetFlag::Hard => "HARD",
      ClusterResetFlag::Soft => "SOFT",
    })
  }
}

/// Flags for the CLUSTER SETSLOT command.
///
/// <https://redis.io/commands/cluster-setslot>
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClusterSetSlotState {
  Importing,
  Migrating,
  Stable,
  Node(String),
}

impl ClusterSetSlotState {
  pub(crate) fn to_str(&self) -> (Str, Option<Str>) {
    let (prefix, value) = match *self {
      ClusterSetSlotState::Importing => ("IMPORTING", None),
      ClusterSetSlotState::Migrating => ("MIGRATING", None),
      ClusterSetSlotState::Stable => ("STABLE", None),
      ClusterSetSlotState::Node(ref n) => ("NODE", Some(n.into())),
    };

    (utils::static_str(prefix), value)
  }
}
