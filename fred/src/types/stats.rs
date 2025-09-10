use crate::types::{ClientState, Stats};
use bytes_utils::Str;
use std::collections::HashMap;

/// Usage stats for an individual client.
#[derive(Clone, Debug)]
pub struct ClientUsage {
  /// Stats describing a distribution of total latency as perceived by callers.
  pub total_latency:   Stats,
  /// Stats describing a distribution of network latency for the client.
  pub network_latency: Stats,
  /// The current state of the client.
  pub state:           ClientState,
}

/// Usage stats for a [DynamicPool](crate::clients::DynamicPool).
#[derive(Clone, Debug)]
pub struct PoolStats {
  /// The number of clients not in a connected state.
  pub disconnected: usize,
  /// Usage stats for clients in the pool.
  pub clients:      HashMap<Str, ClientUsage>,
}
