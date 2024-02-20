//! An assembly of utility functions and core structures used to implement batching support within
//! the router.
//!
//! Apart from the core batching functionality, as expressed in `BatchQuery` and
//! `Batch`, there are a series of utility functions for efficiently converting
//! graphql Requests to/from batch representation in a variety of formats: JSON, bytes

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::oneshot;
use tower::BoxError;

use crate::graphql;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::Context;

#[derive(Clone, Debug, Default)]
pub(crate) struct BatchQuery {
    index: usize,
    // Shared Batch
    shared: Arc<Mutex<Batch>>,
}

impl fmt::Display for BatchQuery {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "index: {}, ", self.index)?;
        // Use try_lock. If the shared batch is locked, we won't display it.
        // TODO: Maybe improve to handle the error...?
        let guard = self.shared.try_lock().ok_or(fmt::Error)?;
        write!(f, "size: {}, ", guard.size)?;
        write!(f, "expected: {:?}, ", guard.expected)?;
        write!(f, "seen: {:?}", guard.seen)?;
        for (service, waiters) in guard.waiters.iter() {
            write!(f, ", service: {}, waiters: {}", service, waiters.len())?;
        }
        Ok(())
    }
}

impl BatchQuery {
    pub(crate) fn new(index: usize, shared: Arc<Mutex<Batch>>) -> Self {
        Self {
            index,
            shared,
            ..Default::default()
        }
    }

    pub(crate) fn ready(&self) -> bool {
        self.shared.lock().ready()
    }

    pub(crate) fn finished(&self) -> bool {
        self.shared.lock().finished()
    }

    pub(crate) fn get_waiter(
        &self,
        request: SubgraphRequest,
        body: graphql::Request,
        context: Context,
        service_name: &str,
    ) -> oneshot::Receiver<Result<SubgraphResponse, BoxError>> {
        tracing::info!("getting a waiter for {}", self.index);
        self.shared
            .lock()
            .get_waiter(request, body, context, service_name.to_string())
    }

    pub(crate) fn get_waiters(
        &self,
    ) -> HashMap<
        String,
        Vec<(
            SubgraphRequest,
            graphql::Request,
            Context,
            oneshot::Sender<Result<SubgraphResponse, BoxError>>,
        )>,
    > {
        let mut guard = self.shared.lock();
        guard.finished = true;
        std::mem::take(&mut guard.waiters)
    }

    pub(crate) fn increment_subgraph_seen(&self) {
        let mut shared_guard = self.shared.lock();
        let value = shared_guard.seen.entry(self.index).or_default();
        *value += 1;
    }

    pub(crate) fn set_subgraph_fetches(&self, fetches: usize) {
        let mut shared_guard = self.shared.lock();
        let value = shared_guard.expected.entry(self.index).or_default();
        *value = fetches;
    }
}

#[derive(Debug, Default)]
pub(crate) struct Batch {
    size: usize,
    expected: HashMap<usize, usize>,
    seen: HashMap<usize, usize>,
    waiters: HashMap<
        String,
        Vec<(
            SubgraphRequest,
            graphql::Request,
            Context,
            oneshot::Sender<Result<SubgraphResponse, BoxError>>,
        )>,
    >,
    finished: bool,
}

impl Batch {
    pub(crate) fn new(size: usize) -> Self {
        Self {
            size,
            expected: HashMap::new(),
            seen: HashMap::new(),
            waiters: HashMap::new(),
            finished: false,
        }
    }

    fn ready(&self) -> bool {
        self.expected.len() == self.size && self.expected == self.seen
    }

    fn finished(&self) -> bool {
        self.finished
    }

    fn get_waiter(
        &mut self,
        request: SubgraphRequest,
        body: graphql::Request,
        context: Context,
        service: String,
    ) -> oneshot::Receiver<Result<SubgraphResponse, BoxError>> {
        let (tx, rx) = oneshot::channel();
        let value = self.waiters.entry(service).or_default();
        value.push((request, body, context, tx));
        rx
    }
}
