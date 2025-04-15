use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;

use parking_lot::Mutex;
use parking_lot::MutexGuard;

use crate::ListenAddr;
use crate::services::router::pipeline_handle::PipelineRef;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ConnectionState {
    Active,
    Terminating,
}

/// A ConnectionRef is used to keep track of how many connections we have active. It's associated with an instance of RouterCreator
/// Pipeline ref represents a unique pipeline
#[derive(Clone, Hash, Eq, PartialEq, Debug)]
pub(crate) struct ConnectionRef {
    pub(crate) pipeline_ref: Arc<PipelineRef>,
    pub(crate) address: ListenAddr,
    /// The state of this connection. When we are trying to shut it down, for instance on reload, it will switch to terminating.
    pub(crate) state: ConnectionState,
}

/// A connection handle does the actual tracking of connections
/// Creating a new connection handle will insert a ConnectionRef into a static map.
/// Dropping all connection handles associated with the internal ref will remove the ConnectionRef
/// Clone MUST NOT be implemented for this type. Cloning will make extra copies that when dropped will throw off the global count.
pub(crate) struct ConnectionHandle {
    pub(crate) connection_ref: ConnectionRef,
}

static CONNECTION_COUNTS: OnceLock<Mutex<HashMap<ConnectionRef, u64>>> = OnceLock::new();
pub(crate) fn connection_counts() -> MutexGuard<'static, HashMap<ConnectionRef, u64>> {
    CONNECTION_COUNTS.get_or_init(Default::default).lock()
}

impl ConnectionHandle {
    pub(crate) fn new(pipeline_ref: Arc<PipelineRef>, address: ListenAddr) -> Self {
        let connection_ref = ConnectionRef {
            pipeline_ref,
            address,
            state: ConnectionState::Active,
        };
        Self::increment(&mut connection_counts(), &connection_ref);
        ConnectionHandle { connection_ref }
    }

    pub(crate) fn shutdown(&mut self) {
        // We obtain the guard across decrement and increment so that telemetry sees this as atomic
        let mut connections = connection_counts();
        Self::decrement(&mut connections, &self.connection_ref);
        self.connection_ref.state = ConnectionState::Terminating;
        Self::increment(&mut connections, &self.connection_ref);
    }

    fn increment(
        connections: &mut MutexGuard<HashMap<ConnectionRef, u64>>,
        connection_ref: &ConnectionRef,
    ) {
        connections
            .entry(connection_ref.clone())
            .and_modify(|p| *p += 1)
            .or_insert(1);
    }

    fn decrement(
        connections: &mut MutexGuard<HashMap<ConnectionRef, u64>>,
        connection_ref: &ConnectionRef,
    ) {
        let value = connections
            .get_mut(connection_ref)
            .expect("connection_ref MUST be greater than zero");
        *value -= 1;
        if *value == 0 {
            connections.remove(connection_ref);
        }
    }
}

impl Drop for ConnectionHandle {
    fn drop(&mut self) {
        Self::decrement(&mut connection_counts(), &self.connection_ref);
    }
}

pub(crate) const OPEN_CONNECTIONS_METRIC: &str = "apollo.router.open_connections";
