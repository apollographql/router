use fred::types::config::{ReplicaFilter, Server};
use parking_lot::RwLock;
use tokio::time::Instant;
use tracing::{debug, warn};

use std::{collections::HashMap, sync::Arc, time::Duration};

/// Filters calls to replicas based on a filter() fn that returns true if there's a routeable
/// replica in the replicas cache. Replicas are routeable when we're able to make a TCP Connection
/// to them. We cache whether we were able to connect for 5 minutes. This applies to each replica,
/// not all replicas as a unit, so we can have some replicas fail and yet still route to the others
///
/// NOTE: filtering happens before any actual connections are made by our redis client (fred), so
/// we shouldn't see any connections errors from replicas that have been filtered out
#[derive(Default, Debug)]
pub(crate) struct RouteableReplicaFilter {
    replicas: Arc<RwLock<HashMap<String, Replica>>>,
}

#[derive(Debug)]
struct Replica {
    expires: Instant,
    routeable: bool,
}

#[async_trait::async_trait]
impl ReplicaFilter for RouteableReplicaFilter {
    // WARN: this is a hot path for fred, keep the instrumentation to trace-level
    #[tracing::instrument(level = "trace")]
    async fn filter(&self, _primary: &Server, replica: &Server) -> bool {
        let addr = format!("{}:{}", replica.host, replica.port);
        // guard block so we drop the read guard before crossing the await boundary below (RwLock
        // not Send)
        let cached = {
            let replicas = self.replicas.read();
            replicas.get(&addr).map(|rep| (rep.expires, rep.routeable))
        };

        // if we have a replica
        if let Some((expires, routeable)) = cached {
            // that hasn't expired yet
            if expires > Instant::now() {
                // return its saved routeability
                return routeable;
            }
            debug!("redis replica filter cache: entry for {addr} expired");
        }

        // otherwise, we try to test routeability via tcp connect
        let routeable = tokio::time::timeout(
            // with a short timeout, 250ms
            Duration::from_millis(250),
            tokio::net::TcpStream::connect(&addr),
        )
        .await
        .map(|res| res.is_ok())
        .inspect_err(|_e| debug!("{addr} is being broadcast as part of redis but is not currently routeable, which may be intentional if using centralized or high-availabitliy setups with internal IPs for certain nodes or might represent a misconfiguration or infrastructure failure"))
        .unwrap_or(false);

        let mut replicas = self.replicas.write();
        replicas.insert(
            addr,
            Replica {
                // 5 minute cache
                expires: Instant::now() + Duration::from_secs(300),
                routeable,
            },
        );

        routeable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    fn dummy_primary() -> Server {
        Server::new("127.0.0.1", 6379)
    }

    fn server(port: u16) -> Server {
        Server::new("127.0.0.1", port)
    }

    fn seed_cache(filter: &RouteableReplicaFilter, port: u16, routeable: bool, expires: Instant) {
        let addr = format!("127.0.0.1:{port}");
        filter
            .replicas
            .write()
            .insert(addr, Replica { expires, routeable });
    }

    #[tokio::test]
    async fn reachable_replica_returns_true() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let filter = RouteableReplicaFilter::default();
        assert!(filter.filter(&dummy_primary(), &server(port)).await);
    }

    #[tokio::test]
    async fn unreachable_replica_returns_false() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let filter = RouteableReplicaFilter::default();
        assert!(!filter.filter(&dummy_primary(), &server(port)).await);
    }

    #[tokio::test]
    async fn cached_result_is_returned_without_reconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let filter = RouteableReplicaFilter::default();
        assert!(filter.filter(&dummy_primary(), &server(port)).await);

        // drop the listener — port is now unreachable
        drop(listener);

        // should still return true from cache
        assert!(filter.filter(&dummy_primary(), &server(port)).await);
    }

    #[tokio::test]
    async fn result_is_cached_after_filter() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let filter = RouteableReplicaFilter::default();
        filter.filter(&dummy_primary(), &server(port)).await;

        let replicas = filter.replicas.read();
        let addr = format!("127.0.0.1:{port}");
        let entry = replicas.get(&addr).expect("entry should be cached");
        assert!(entry.routeable);
        assert!(entry.expires > Instant::now());
    }

    #[tokio::test]
    async fn expired_cache_triggers_fresh_connect() {
        // seed with an already-expired entry that says routeable=true
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let filter = RouteableReplicaFilter::default();
        let expired = Instant::now() - Duration::from_secs(1);
        seed_cache(&filter, port, true, expired);

        // cache says true, but it's expired — fresh connect will fail
        assert!(!filter.filter(&dummy_primary(), &server(port)).await);
    }

    #[tokio::test]
    async fn unexpired_cache_is_used() {
        // seed with a not-yet-expired entry, no listener needed
        let filter = RouteableReplicaFilter::default();
        let port = 1; // doesn't matter, cache will be hit
        let future = Instant::now() + Duration::from_secs(300);
        seed_cache(&filter, port, true, future);

        assert!(filter.filter(&dummy_primary(), &server(port)).await);
    }

    #[tokio::test]
    async fn unexpired_false_cache_is_used() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        // seed with routeable=false even though the port is actually reachable
        let filter = RouteableReplicaFilter::default();
        let future = Instant::now() + Duration::from_secs(300);
        seed_cache(&filter, port, false, future);

        // cache wins — returns false despite the port being open
        assert!(!filter.filter(&dummy_primary(), &server(port)).await);
    }

    #[tokio::test]
    async fn separate_replicas_cached_independently() {
        let listener_a = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port_a = listener_a.local_addr().unwrap().port();

        let listener_b = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port_b = listener_b.local_addr().unwrap().port();
        drop(listener_b);

        let filter = RouteableReplicaFilter::default();
        assert!(filter.filter(&dummy_primary(), &server(port_a)).await);
        assert!(!filter.filter(&dummy_primary(), &server(port_b)).await);

        let replicas = filter.replicas.read();
        assert_eq!(replicas.len(), 2);
    }

    #[tokio::test]
    async fn expired_entry_gets_replaced() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let filter = RouteableReplicaFilter::default();
        let expired = Instant::now() - Duration::from_secs(1);
        seed_cache(&filter, port, false, expired);

        // expired false entry should be replaced by a fresh true result
        assert!(filter.filter(&dummy_primary(), &server(port)).await);

        let replicas = filter.replicas.read();
        let addr = format!("127.0.0.1:{port}");
        let entry = replicas.get(&addr).unwrap();
        assert!(entry.routeable);
        assert!(entry.expires > Instant::now());
    }

    #[tokio::test]
    async fn connect_timeout_returns_false() {
        let filter = RouteableReplicaFilter::default();
        let primary = dummy_primary();
        // this is a special non-routeable address (designated for use in documentaton/examples),
        // rfc 5737, but if this flakes a bunch in ci/cd, we can mark it ignore and just keep it
        // locally
        let replica = Server::new("192.0.2.1", 1);

        let start = Instant::now();
        let result = filter.filter(&primary, &replica).await;
        let elapsed = start.elapsed();

        assert!(!result);
        // Should have waited for the 250ms timeout, not returned instantly
        assert!(
            elapsed >= Duration::from_millis(200),
            "expected timeout (~250ms), but returned in {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn connect_timeout_result_is_cached() {
        let filter = RouteableReplicaFilter::default();
        let primary = dummy_primary();
        let replica = Server::new("192.0.2.1", 1);

        filter.filter(&primary, &replica).await;

        // Second call should return from cache instantly, not wait for timeout
        let start = Instant::now();
        let result = filter.filter(&primary, &replica).await;
        let elapsed = start.elapsed();

        assert!(!result);
        assert!(
            elapsed < Duration::from_millis(50),
            "expected instant cache hit, but took {elapsed:?}"
        );
    }
}
