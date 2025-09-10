use crate::{
  commands,
  error::Error,
  interfaces::{ClientLike, FredResult},
  protocol::types::ClusterRouting,
  types::{
    cluster::{ClusterFailoverFlag, ClusterResetFlag, ClusterSetSlotState},
    FromValue,
    Key,
    MultipleHashSlots,
  },
};
use bytes_utils::Str;
use fred_macros::rm_send_if;
use futures::Future;

/// Functions that implement the [cluster](https://redis.io/commands#cluster) interface.
#[rm_send_if(feature = "glommio")]
pub trait ClusterInterface: ClientLike + Sized {
  /// Read the cached cluster state used for routing commands to the correct cluster nodes.
  fn cached_cluster_state(&self) -> Option<ClusterRouting> {
    self.inner().with_cluster_state(|state| Ok(state.clone())).ok()
  }

  /// Read the number of known primary cluster nodes, or `0` if the cluster state is not known.
  fn num_primary_cluster_nodes(&self) -> usize {
    self
      .inner()
      .with_cluster_state(|state| Ok(state.unique_primary_nodes().len()))
      .unwrap_or(0)
  }

  /// Update the cached cluster state and add or remove any changed cluster node connections.
  fn sync_cluster(&self) -> impl Future<Output = Result<(), Error>> + Send {
    async move { commands::cluster::sync_cluster(self).await }
  }

  /// Advances the cluster config epoch.
  ///
  /// <https://redis.io/commands/cluster-bumpepoch>
  fn cluster_bumpepoch<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::cluster::cluster_bumpepoch(self).await?.convert() }
  }

  /// Deletes all slots from a node.
  ///
  /// <https://redis.io/commands/cluster-flushslots>
  fn cluster_flushslots(&self) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::cluster::cluster_flushslots(self).await }
  }

  /// Returns the node's id.
  ///
  /// <https://redis.io/commands/cluster-myid>
  fn cluster_myid<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::cluster::cluster_myid(self).await?.convert() }
  }

  /// Read the current cluster node configuration.
  ///
  /// Note: The client keeps a cached, parsed version of the cluster state in memory available at
  /// [cached_cluster_state](Self::cached_cluster_state).
  ///
  /// <https://redis.io/commands/cluster-nodes>
  fn cluster_nodes<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::cluster::cluster_nodes(self).await?.convert() }
  }

  /// Forces a node to save the nodes.conf configuration on disk.
  ///
  /// <https://redis.io/commands/cluster-saveconfig>
  fn cluster_saveconfig(&self) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::cluster::cluster_saveconfig(self).await }
  }

  /// CLUSTER SLOTS returns details about which cluster slots map to which Redis instances.
  ///
  /// <https://redis.io/commands/cluster-slots>
  fn cluster_slots<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::cluster::cluster_slots(self).await?.convert() }
  }

  /// CLUSTER INFO provides INFO style information about Redis Cluster vital parameters.
  ///
  /// <https://redis.io/commands/cluster-info>
  fn cluster_info<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::cluster::cluster_info(self).await?.convert() }
  }

  /// This command is useful in order to modify a node's view of the cluster configuration. Specifically it assigns a
  /// set of hash slots to the node receiving the command.
  ///
  /// <https://redis.io/commands/cluster-addslots>
  fn cluster_add_slots<S>(&self, slots: S) -> impl Future<Output = FredResult<()>> + Send
  where
    S: Into<MultipleHashSlots> + Send,
  {
    async move {
      into!(slots);
      commands::cluster::cluster_add_slots(self, slots).await
    }
  }

  /// The command returns the number of failure reports for the specified node.
  ///
  /// <https://redis.io/commands/cluster-count-failure-reports>
  fn cluster_count_failure_reports<R, S>(&self, node_id: S) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Str> + Send,
  {
    async move {
      into!(node_id);
      commands::cluster::cluster_count_failure_reports(self, node_id)
        .await?
        .convert()
    }
  }

  /// Returns the number of keys in the specified Redis Cluster hash slot.
  ///
  /// <https://redis.io/commands/cluster-countkeysinslot>
  fn cluster_count_keys_in_slot<R>(&self, slot: u16) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move {
      commands::cluster::cluster_count_keys_in_slot(self, slot)
        .await?
        .convert()
    }
  }

  /// The CLUSTER DELSLOTS command asks a particular Redis Cluster node to forget which master is serving the hash
  /// slots specified as arguments.
  ///
  /// <https://redis.io/commands/cluster-delslots>
  fn cluster_del_slots<S>(&self, slots: S) -> impl Future<Output = FredResult<()>> + Send
  where
    S: Into<MultipleHashSlots> + Send,
  {
    async move {
      into!(slots);
      commands::cluster::cluster_del_slots(self, slots).await
    }
  }

  /// This command, that can only be sent to a Redis Cluster replica node, forces the replica to start a manual
  /// failover of its master instance.
  ///
  /// <https://redis.io/commands/cluster-failover>
  fn cluster_failover(&self, flag: Option<ClusterFailoverFlag>) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::cluster::cluster_failover(self, flag).await }
  }

  /// The command is used in order to remove a node, specified via its node ID, from the set of known nodes of the
  /// Redis Cluster node receiving the command. In other words the specified node is removed from the nodes table of
  /// the node receiving the command.
  ///
  /// <https://redis.io/commands/cluster-forget>
  fn cluster_forget<S>(&self, node_id: S) -> impl Future<Output = FredResult<()>> + Send
  where
    S: Into<Str> + Send,
  {
    async move {
      into!(node_id);
      commands::cluster::cluster_forget(self, node_id).await
    }
  }

  /// The command returns an array of keys names stored in the contacted node and hashing to the specified hash slot.
  ///
  /// <https://redis.io/commands/cluster-getkeysinslot>
  fn cluster_get_keys_in_slot<R>(&self, slot: u16, count: u64) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move {
      commands::cluster::cluster_get_keys_in_slot(self, slot, count)
        .await?
        .convert()
    }
  }

  /// Returns an integer identifying the hash slot the specified key hashes to.
  ///
  /// <https://redis.io/commands/cluster-keyslot>
  fn cluster_keyslot<R, K>(&self, key: K) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
  {
    async move {
      into!(key);
      commands::cluster::cluster_keyslot(self, key).await?.convert()
    }
  }

  /// CLUSTER MEET is used in order to connect different Redis nodes with cluster support enabled, into a working
  /// cluster.
  ///
  /// <https://redis.io/commands/cluster-meet>
  fn cluster_meet<S>(&self, ip: S, port: u16) -> impl Future<Output = FredResult<()>> + Send
  where
    S: Into<Str> + Send,
  {
    async move {
      into!(ip);
      commands::cluster::cluster_meet(self, ip, port).await
    }
  }

  /// The command reconfigures a node as a replica of the specified master. If the node receiving the command is an
  /// empty master, as a side effect of the command, the node role is changed from master to replica.
  ///
  /// <https://redis.io/commands/cluster-replicate>
  fn cluster_replicate<S>(&self, node_id: S) -> impl Future<Output = FredResult<()>> + Send
  where
    S: Into<Str> + Send,
  {
    async move {
      into!(node_id);
      commands::cluster::cluster_replicate(self, node_id).await
    }
  }

  /// The command provides a list of replica nodes replicating from the specified master node.
  ///
  /// <https://redis.io/commands/cluster-replicas>
  fn cluster_replicas<R, S>(&self, node_id: S) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Str> + Send,
  {
    async move {
      into!(node_id);
      commands::cluster::cluster_replicas(self, node_id).await?.convert()
    }
  }

  /// Reset a Redis Cluster node, in a more or less drastic way depending on the reset type, that can be hard or soft.
  /// Note that this command does not work for masters if they hold one or more keys, in that case to completely
  /// reset a master node keys must be removed first, e.g. by using FLUSHALL first, and then CLUSTER RESET.
  ///
  /// <https://redis.io/commands/cluster-reset>
  fn cluster_reset(&self, mode: Option<ClusterResetFlag>) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::cluster::cluster_reset(self, mode).await }
  }

  /// This command sets a specific config epoch in a fresh node.
  ///
  /// <https://redis.io/commands/cluster-set-config-epoch>
  fn cluster_set_config_epoch(&self, epoch: u64) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::cluster::cluster_set_config_epoch(self, epoch).await }
  }

  /// CLUSTER SETSLOT is responsible for changing the state of a hash slot in the receiving node in different ways.
  ///
  /// <https://redis.io/commands/cluster-setslot>
  fn cluster_setslot(&self, slot: u16, state: ClusterSetSlotState) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::cluster::cluster_setslot(self, slot, state).await }
  }
}
