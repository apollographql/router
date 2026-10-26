use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use fred::prelude::ClientLike;
use fred::types::Builder;
use fred::types::config::Config;
use fred::types::config::ReplicaFilter;
use fred::types::config::Server;
use fred::types::config::ServerConfig;
use parking_lot::RwLock;
use tokio::time::Instant;
use tracing::debug;

/// How long a replica that failed its probe is left out of replica routing.
const UNROUTEABLE_FOR: Duration = Duration::from_secs(300);

/// Filters the replicas fred routes to down to the ones that can serve commands, so that one
/// broken replica can't fail the whole client.
///
/// In cluster mode fred connects to every replica it's allowed to route to while it syncs the
/// cluster, and a replica that fails to connect fails the sync, and with it the client (see
/// `configure_cluster_replicas`). fred calls this filter for each replica right before that
/// connection attempt, so we probe the replica by opening a connection the way fred will: with the
/// pool's credentials and TLS settings, running the same handshake, and giving up after the Redis
/// timeout. A replica that refuses connections, drops them, or accepts them and never answers (for
/// example a paused replica) fails the probe and is left out, and reads go to the other replicas,
/// or to the primary when there are none.
///
/// A replica that fails is left out for 5 minutes, so repeated syncs while it's broken don't each
/// wait on it. A passing probe isn't cached: fred re-syncs replicas when a replica connection breaks,
/// and a replica that just broke must be probed again rather than trusted.
///
/// NOTE: a replica that's left out is only added back when fred next syncs replicas, which happens
/// when a connection breaks or the cluster topology changes, not on a timer.
#[derive(Debug)]
pub(crate) struct RouteableReplicaFilter {
    /// The pool's configuration, which the probe connects with.
    config: Config,
    timeout: Duration,
    /// Replicas that failed their probe, with when they may be probed again.
    unrouteable: Arc<RwLock<HashMap<String, Instant>>>,
}

impl RouteableReplicaFilter {
    pub(crate) fn new(config: Config, timeout: Duration) -> Self {
        Self {
            config,
            timeout,
            unrouteable: Default::default(),
        }
    }

    /// Connects to the replica and completes fred's connection handshake within the timeout.
    async fn probe(&self, replica: &Server) -> bool {
        let mut config = self.config.clone();
        config.server = ServerConfig::Centralized {
            server: replica.clone(),
        };
        let client = match Builder::from_config(config)
            .with_connection_config(|config| {
                config.connection_timeout = self.timeout;
                config.internal_command_timeout = self.timeout;
            })
            .build()
        {
            Ok(client) => client,
            Err(err) => {
                debug!("unable to build a probe client for redis replica {replica}: {err}");
                return false;
            }
        };

        // subscribe before connecting so that the connection result can't be missed
        let mut connected = client.inner().notifications.connect.load().subscribe();
        let connection_task = client.connect();
        let result = tokio::time::timeout(self.timeout, connected.recv()).await;
        // closes the probe connection, or abandons the attempt if it timed out
        connection_task.abort();

        match result {
            Ok(Ok(Ok(()))) => true,
            Ok(Ok(Err(err))) => {
                debug!("redis replica {replica} failed its probe: {err}");
                false
            }
            Ok(Err(err)) => {
                debug!("redis replica {replica} probe ended without a result: {err}");
                false
            }
            Err(_) => {
                debug!(
                    "redis replica {replica} didn't answer its probe within {:?}",
                    self.timeout
                );
                false
            }
        }
    }
}

#[async_trait::async_trait]
impl ReplicaFilter for RouteableReplicaFilter {
    #[tracing::instrument(level = "trace", skip(self))]
    async fn filter(&self, _primary: &Server, replica: &Server) -> bool {
        let addr = format!("{}:{}", replica.host, replica.port);
        let unrouteable_until = self.unrouteable.read().get(&addr).copied();
        if unrouteable_until.is_some_and(|until| until > Instant::now()) {
            return false;
        }

        let routeable = self.probe(replica).await;
        if routeable {
            self.unrouteable.write().remove(&addr);
        } else {
            debug!(
                "redis replica {addr} failed its probe and won't receive reads for {UNROUTEABLE_FOR:?}"
            );
            self.unrouteable
                .write()
                .insert(addr, Instant::now() + UNROUTEABLE_FOR);
        }
        routeable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::redis::fake_redis::Behaviour;
    use crate::cache::redis::fake_redis::FakeRedis;
    use crate::cache::redis::fake_redis::Topology;

    const TIMEOUT: Duration = Duration::from_millis(500);

    fn filter() -> RouteableReplicaFilter {
        RouteableReplicaFilter::new(Config::default(), TIMEOUT)
    }

    fn server(port: u16) -> Server {
        Server::new("127.0.0.1", port)
    }

    /// Starts a deployment with one replica that behaves as given, and returns it with the
    /// replica's port.
    async fn replica(behaviour: Behaviour) -> (FakeRedis, u16) {
        let redis = FakeRedis::start(Topology::Centralized, &[behaviour]).await;
        let port = redis.replicas[0].port;
        (redis, port)
    }

    fn is_unrouteable(filter: &RouteableReplicaFilter, port: u16) -> bool {
        filter
            .unrouteable
            .read()
            .contains_key(&format!("127.0.0.1:{port}"))
    }

    fn mark_unrouteable(filter: &RouteableReplicaFilter, port: u16, until: Instant) {
        filter
            .unrouteable
            .write()
            .insert(format!("127.0.0.1:{port}"), until);
    }

    /// A passing probe isn't remembered, so the replica is probed again on the next sync.
    #[tokio::test]
    async fn healthy_replica_is_routeable() {
        let (_redis, port) = replica(Behaviour::Healthy).await;
        let filter = filter();

        assert!(filter.filter(&server(1), &server(port)).await);
        assert!(!is_unrouteable(&filter, port));
    }

    #[tokio::test]
    #[rstest::rstest]
    async fn broken_replica_is_not_routeable(
        #[values(Behaviour::Refusing, Behaviour::Dropping, Behaviour::Unresponsive)]
        behaviour: Behaviour,
    ) {
        let (_redis, port) = replica(behaviour).await;
        let filter = filter();

        let start = Instant::now();
        assert!(!filter.filter(&server(1), &server(port)).await);
        assert!(
            start.elapsed() < TIMEOUT * 2,
            "the probe should give up after the timeout"
        );
        assert!(is_unrouteable(&filter, port));
    }

    #[tokio::test]
    async fn unrouteable_replica_is_not_probed_again_until_it_expires() {
        let (redis, port) = replica(Behaviour::Healthy).await;
        let filter = filter();

        mark_unrouteable(&filter, port, Instant::now() + UNROUTEABLE_FOR);
        assert!(!filter.filter(&server(1), &server(port)).await);
        assert_eq!(redis.replicas[0].stats.connections(), 0);

        mark_unrouteable(&filter, port, Instant::now() - Duration::from_secs(1));
        assert!(filter.filter(&server(1), &server(port)).await);
        assert!(!is_unrouteable(&filter, port));
    }
}
