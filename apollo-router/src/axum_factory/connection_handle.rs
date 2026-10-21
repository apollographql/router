use std::sync::Arc;

use crate::ListenAddr;
use crate::metrics::UpDownCounterGuard;
use crate::services::router::pipeline_handle::PipelineHandle;
use crate::services::router::pipeline_handle::PipelineRef;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ConnectionState {
    Active,
    Terminating,
}

/// A connection handle does the actual tracking of connections
/// Creating a new connection handle will increment the updown counter.
/// Dropping the connection handle will decrement the updown counter
/// Clone MUST NOT be implemented for this type. Cloning will make extra copies that when dropped will throw off the count.
pub(crate) struct ConnectionHandle {
    pub(crate) pipeline_ref: Arc<PipelineRef>,
    pub(crate) address: ListenAddr,
    state: ConnectionState,
    guard: UpDownCounterGuard<i64>,
    /// Keeps the pipeline's `apollo.router.pipelines` gauge from dropping to 0
    /// while this connection is still open. The gauge is otherwise only tied
    /// to the `RouterFactory` generation's own lifetime, which a config reload
    /// can drop well before connections created under it finish draining.
    _pipeline_handle: Arc<PipelineHandle>,
}

impl ConnectionHandle {
    pub(crate) fn new(pipeline_handle: Arc<PipelineHandle>, address: ListenAddr) -> Self {
        let pipeline_ref = pipeline_handle.pipeline_ref.clone();
        let guard = Self::create_counter_guard(&pipeline_ref, &address, ConnectionState::Active);

        ConnectionHandle {
            pipeline_ref,
            address,
            state: ConnectionState::Active,
            guard,
            _pipeline_handle: pipeline_handle,
        }
    }

    pub(crate) fn shutdown(&mut self) {
        if self.state != ConnectionState::Terminating {
            self.state = ConnectionState::Terminating;
            // Replace the guard with a new one for terminating state
            self.guard = Self::create_counter_guard(
                &self.pipeline_ref,
                &self.address,
                ConnectionState::Terminating,
            );
        }
    }

    fn create_counter_guard(
        pipeline_ref: &Arc<PipelineRef>,
        address: &ListenAddr,
        state: ConnectionState,
    ) -> UpDownCounterGuard<i64> {
        use opentelemetry::KeyValue;

        let state_str = match state {
            ConnectionState::Active => "active",
            ConnectionState::Terminating => "terminating",
        };

        let mut attributes = Vec::with_capacity(6);

        if let Some((ip, port)) = address.ip_and_port() {
            attributes.push(KeyValue::new("server.address", ip.to_string()));
            attributes.push(KeyValue::new("server.port", port.to_string()));
        } else {
            attributes.push(KeyValue::new("server.address", address.to_string()));
        }

        attributes.push(KeyValue::new("schema.id", pipeline_ref.schema_id.clone()));
        attributes.push(KeyValue::new(
            "launch.id",
            pipeline_ref.launch_id.clone().unwrap_or_default(),
        ));
        attributes.push(KeyValue::new(
            "config.hash",
            pipeline_ref.config_hash.clone(),
        ));
        attributes.push(KeyValue::new("http.connection.state", state_str));

        i64_up_down_counter_with_unit!(
            "apollo.router.open_connections",
            "Number of currently connected clients",
            "{connection}",
            1,
            attributes
        )
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::*;
    use crate::metrics::FutureMetricsExt;

    #[tokio::test]
    async fn test_connection_handle_increments_counter() {
        async {
            let pipeline_handle = Arc::new(PipelineHandle::new(
                "schema1".to_string(),
                Some("launch1".to_string()),
                "config1".to_string(),
            ));

            let addr = ListenAddr::SocketAddr(SocketAddr::from(([127, 0, 0, 1], 4000)));
            let _handle = ConnectionHandle::new(pipeline_handle, addr);

            assert_up_down_counter!(
                "apollo.router.open_connections",
                1,
                "server.address" = "127.0.0.1",
                "server.port" = "4000",
                "schema.id" = "schema1",
                "launch.id" = "launch1",
                "config.hash" = "config1",
                "http.connection.state" = "active"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_connection_handle_decrements_on_drop() {
        async {
            let pipeline_handle = Arc::new(PipelineHandle::new(
                "schema1".to_string(),
                Some("launch1".to_string()),
                "config1".to_string(),
            ));

            let addr = ListenAddr::SocketAddr(SocketAddr::from(([127, 0, 0, 1], 4000)));

            {
                let _handle = ConnectionHandle::new(pipeline_handle.clone(), addr.clone());

                assert_up_down_counter!(
                    "apollo.router.open_connections",
                    1,
                    "server.address" = "127.0.0.1",
                    "server.port" = "4000",
                    "schema.id" = "schema1",
                    "launch.id" = "launch1",
                    "config.hash" = "config1",
                    "http.connection.state" = "active"
                );
            }

            // After dropping, counter should be back to 0
            assert_up_down_counter!(
                "apollo.router.open_connections",
                0,
                "server.address" = "127.0.0.1",
                "server.port" = "4000",
                "schema.id" = "schema1",
                "launch.id" = "launch1",
                "config.hash" = "config1",
                "http.connection.state" = "active"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_connection_handle_shutdown_changes_state() {
        async {
            let pipeline_handle = Arc::new(PipelineHandle::new(
                "schema1".to_string(),
                Some("launch1".to_string()),
                "config1".to_string(),
            ));

            let addr = ListenAddr::SocketAddr(SocketAddr::from(([127, 0, 0, 1], 4000)));
            let mut handle = ConnectionHandle::new(pipeline_handle, addr);

            // Initially active
            assert_up_down_counter!(
                "apollo.router.open_connections",
                1,
                "server.address" = "127.0.0.1",
                "server.port" = "4000",
                "schema.id" = "schema1",
                "launch.id" = "launch1",
                "config.hash" = "config1",
                "http.connection.state" = "active"
            );

            // Shutdown changes to terminating
            handle.shutdown();

            assert_up_down_counter!(
                "apollo.router.open_connections",
                0,
                "server.address" = "127.0.0.1",
                "server.port" = "4000",
                "schema.id" = "schema1",
                "launch.id" = "launch1",
                "config.hash" = "config1",
                "http.connection.state" = "active"
            );

            assert_up_down_counter!(
                "apollo.router.open_connections",
                1,
                "server.address" = "127.0.0.1",
                "server.port" = "4000",
                "schema.id" = "schema1",
                "launch.id" = "launch1",
                "config.hash" = "config1",
                "http.connection.state" = "terminating"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_connection_handle_multiple_connections() {
        async {
            let pipeline_handle = Arc::new(PipelineHandle::new(
                "schema1".to_string(),
                None,
                "config1".to_string(),
            ));

            let addr1 = ListenAddr::SocketAddr(SocketAddr::from(([127, 0, 0, 1], 4000)));
            let addr2 = ListenAddr::SocketAddr(SocketAddr::from(([127, 0, 0, 1], 4001)));

            let _handle1 = ConnectionHandle::new(pipeline_handle.clone(), addr1);
            let _handle2 = ConnectionHandle::new(pipeline_handle, addr2);

            // Check first connection
            assert_up_down_counter!(
                "apollo.router.open_connections",
                1,
                "server.address" = "127.0.0.1",
                "server.port" = "4000",
                "schema.id" = "schema1",
                "launch.id" = "",
                "config.hash" = "config1",
                "http.connection.state" = "active"
            );

            // Check second connection
            assert_up_down_counter!(
                "apollo.router.open_connections",
                1,
                "server.address" = "127.0.0.1",
                "server.port" = "4001",
                "schema.id" = "schema1",
                "launch.id" = "",
                "config.hash" = "config1",
                "http.connection.state" = "active"
            );
        }
        .with_metrics()
        .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_connection_handle_unix_socket() {
        async {
            let pipeline_handle = Arc::new(PipelineHandle::new(
                "schema1".to_string(),
                Some("launch1".to_string()),
                "config1".to_string(),
            ));

            let addr = ListenAddr::UnixSocket("/tmp/router.sock".into());
            let _handle = ConnectionHandle::new(pipeline_handle, addr);

            assert_up_down_counter!(
                "apollo.router.open_connections",
                1,
                "server.address" = "/tmp/router.sock",
                "schema.id" = "schema1",
                "launch.id" = "launch1",
                "config.hash" = "config1",
                "http.connection.state" = "active"
            );
        }
        .with_metrics()
        .await;
    }
}
