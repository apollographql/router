use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use fred::prelude::ClientLike;
use fred::types::Builder;
use fred::types::config::Config;
use fred::types::config::ReplicaFilter;
use fred::types::config::Server;
use fred::types::config::ServerConfig;
use futures::FutureExt;
use futures::future::BoxFuture;
use futures::future::Shared;
use parking_lot::Mutex;
use parking_lot::RwLock;
use tokio::time::Instant;
use tracing::debug;
use tracing::warn;

/// How long a replica that failed its probe is left out of replica routing.
const UNROUTEABLE_FOR: Duration = Duration::from_secs(300);

/// A probe of one replica, which every caller asking about that replica while it runs awaits.
type Probe = Shared<BoxFuture<'static, bool>>;

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
/// A replica that fails is left out for at least 5 minutes, so repeated syncs while it's broken
/// don't each wait on it. A passing probe isn't cached: fred re-syncs replicas when a replica
/// connection breaks, and a replica that just broke must be probed again rather than trusted.
///
/// Every client in a pool shares one filter, and they tend to sync at the same moment (when a
/// replica stalls, every client's heartbeat ping to it fails together), so callers asking about a
/// replica that's already being probed wait for that probe rather than starting their own.
///
/// NOTE: fred only asks the filter about a replica when it syncs replicas, so a left-out replica
/// is only probed again at a sync. Syncs happen when a connection breaks or the cluster topology
/// changes, and the replica heartbeat starts one once a left-out replica's 5 minutes are up (see
/// [`Self::take_replicas_to_recheck`]).
pub(crate) struct RouteableReplicaFilter {
    /// The pool's configuration, which the probe connects with.
    config: Config,
    timeout: Duration,
    /// Replicas that failed their probe, with when they may be probed again.
    unrouteable: Arc<RwLock<HashMap<String, Instant>>>,
    /// Probes in progress, by replica address.
    probes: Arc<Mutex<HashMap<String, Probe>>>,
}

impl RouteableReplicaFilter {
    pub(crate) fn new(config: Config, timeout: Duration) -> Self {
        Self {
            config,
            timeout,
            unrouteable: Default::default(),
            probes: Default::default(),
        }
    }

    /// Forgets the left-out replicas that have waited out their period, and returns whether there
    /// were any, in which case the next replica sync should run soon.
    ///
    /// A forgotten replica that's still listed is probed at that sync, and left out again if it
    /// fails. One the cluster no longer lists (a replaced pod, a removed replica) is never asked
    /// about again, so its entry must go here or it would call for a sync at every heartbeat.
    pub(crate) fn take_replicas_to_recheck(&self) -> bool {
        let now = Instant::now();
        let mut unrouteable = self.unrouteable.write();
        let left_out = unrouteable.len();
        unrouteable.retain(|_, until| *until > now);
        unrouteable.len() < left_out
    }

    /// Ends the period every left-out replica must wait before it's probed again.
    #[cfg(test)]
    pub(crate) fn end_left_out_period(&self) {
        let now = Instant::now();
        for until in self.unrouteable.write().values_mut() {
            *until = now;
        }
    }

    /// Returns the probe in progress for the replica, starting one if there isn't one. The probe
    /// records its result before it completes.
    fn probe(&self, addr: &str, replica: &Server) -> Probe {
        self.probes
            .lock()
            .entry(addr.to_string())
            .or_insert_with(|| {
                let probe = probe(self.config.clone(), self.timeout, replica.clone());
                let unrouteable = self.unrouteable.clone();
                let probes = self.probes.clone();
                let addr = addr.to_string();
                async move {
                    let routeable = probe.await;
                    if routeable {
                        unrouteable.write().remove(&addr);
                    } else {
                        // once per failed probe, which is at most once per replica per period
                        warn!(
                            "redis replica {addr} failed its probe and won't receive reads until it's probed again in {UNROUTEABLE_FOR:?}"
                        );
                        unrouteable
                            .write()
                            .insert(addr.clone(), Instant::now() + UNROUTEABLE_FOR);
                    }
                    // after recording the result, so a caller arriving now sees it
                    probes.lock().remove(&addr);
                    routeable
                }
                .boxed()
                .shared()
            })
            .clone()
    }
}

/// Connects to the replica and completes fred's connection handshake within the timeout.
async fn probe(mut config: Config, timeout: Duration, replica: Server) -> bool {
    config.server = ServerConfig::Centralized {
        server: replica.clone(),
    };
    let client = match Builder::from_config(config)
        .with_connection_config(|config| {
            config.connection_timeout = timeout;
            config.internal_command_timeout = timeout;
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
    let result = tokio::time::timeout(timeout, connected.recv()).await;
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
            debug!("redis replica {replica} didn't answer its probe within {timeout:?}");
            false
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

        self.probe(&addr, replica).await
    }
}

#[cfg(test)]
mod tests {
    use futures::future::join_all;

    use super::*;
    use crate::cache::redis::fake_redis::Behaviour;
    use crate::cache::redis::fake_redis::FakeRedis;
    use crate::cache::redis::fake_redis::Topology;
    use crate::test_harness::tracing_test;

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

    /// Operators see one warning when a replica is left out, however many clients asked about it.
    #[tokio::test]
    async fn left_out_replica_is_logged_once_as_a_warning() {
        let _guard = tracing_test::dispatcher_guard();
        let (_redis, port) = replica(Behaviour::Unresponsive).await;
        let filter = filter();

        let (primary, replica) = (server(1), server(port));
        join_all((0..4).map(|_| filter.filter(&primary, &replica))).await;

        let left_out = format!(
            "redis replica 127.0.0.1:{port} failed its probe and won't receive reads until it's \
             probed again in {UNROUTEABLE_FOR:?}"
        );
        tracing_test::logs_assert(|lines| {
            let warnings: Vec<_> = lines
                .iter()
                .filter(|line| line.contains(&left_out))
                .collect();
            match warnings.as_slice() {
                [line] if line.contains("WARN") => Ok(()),
                _ => Err(format!("expected one warning, got {warnings:?}")),
            }
        })
        .unwrap();
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

    /// Every client in a pool shares the filter and re-syncs at the same heartbeat, so they must
    /// share one probe of a broken replica rather than each opening a connection and waiting on it.
    #[tokio::test]
    async fn concurrent_probes_of_a_replica_share_one_result() {
        let (redis, port) = replica(Behaviour::Unresponsive).await;
        let filter = filter();

        let (primary, replica) = (server(1), server(port));
        let start = Instant::now();
        let results = join_all((0..4).map(|_| filter.filter(&primary, &replica))).await;
        assert_eq!(results, [false; 4]);
        assert_eq!(redis.replicas[0].stats.connections(), 1);
        assert!(
            start.elapsed() < TIMEOUT * 2,
            "the probes should finish after one timeout"
        );
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

    /// The replica heartbeat starts a sync once this is true, which is what brings a recovered
    /// replica back.
    #[test]
    fn replica_is_due_a_recheck_once_its_left_out_period_ends() {
        let filter = filter();
        assert!(!filter.take_replicas_to_recheck());

        mark_unrouteable(&filter, 1, Instant::now() + UNROUTEABLE_FOR);
        assert!(!filter.take_replicas_to_recheck());
        assert!(is_unrouteable(&filter, 1));

        filter.end_left_out_period();
        assert!(filter.take_replicas_to_recheck());
    }

    /// fred never asks about a replica the cluster no longer lists, so no probe will clear its
    /// entry. Taking the recheck must, or every heartbeat would re-sync every client.
    #[test]
    fn replica_is_rechecked_once_per_left_out_period() {
        let filter = filter();
        mark_unrouteable(&filter, 1, Instant::now() - Duration::from_secs(1));
        mark_unrouteable(&filter, 2, Instant::now() + UNROUTEABLE_FOR);

        assert!(filter.take_replicas_to_recheck());
        assert!(!filter.take_replicas_to_recheck());
        assert!(!is_unrouteable(&filter, 1));
        assert!(
            is_unrouteable(&filter, 2),
            "replicas still left out are kept"
        );
    }
}
