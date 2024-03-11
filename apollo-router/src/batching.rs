//! An assembly of utility functions and core structures used to implement batching support within
//! the router.
//!
//! In addition to the core batching functionality, as expressed in `BatchQuery` and
//! `Batch`, there are a series of utility functions for efficiently converting
//! graphql Requests to/from batch representation in a variety of formats: JSON, bytes

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use bytes::BufMut;
use bytes::BytesMut;
use hyper::Body;
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
        Self { index, shared }
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

    pub(crate) fn get_waiters(&self) -> HashMap<String, Vec<Waiter>> {
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
    waiters: HashMap<String, Vec<Waiter>>,
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
        value.push(Waiter::new(request, body, context, tx));
        rx
    }
}

#[derive(Debug)]
pub(crate) struct Waiter {
    pub(crate) sg_request: SubgraphRequest,
    pub(crate) gql_request: graphql::Request,
    pub(crate) context: Context,
    pub(crate) sender: oneshot::Sender<Result<SubgraphResponse, BoxError>>,
}

impl Waiter {
    fn new(
        sg_request: SubgraphRequest,
        gql_request: graphql::Request,
        context: Context,
        sender: oneshot::Sender<Result<SubgraphResponse, BoxError>>,
    ) -> Self {
        Self {
            sg_request,
            gql_request,
            context,
            sender,
        }
    }

    // Form a batch from a collection of waiting requests. The first operation provides:
    //  - operation name
    //  - context
    //  - parts
    // This is ok, because when the batch was created, the parts and context were primarily created
    // by extracting and duplicating information from the single batch request. Maybe we should use
    // a different operation name, maybe chain them all together? TODO: Decide operation name
    // DECISION: For now we will ignore the operation name which is extracted here.
    pub(crate) async fn assemble_batch(
        service_waiters: Vec<Waiter>,
    ) -> Result<
        (
            String,
            Context,
            http::Request<Body>,
            Vec<oneshot::Sender<Result<SubgraphResponse, BoxError>>>,
        ),
        BoxError,
    > {
        let mut txs = Vec::with_capacity(service_waiters.len());
        let mut service_waiters_it = service_waiters.into_iter();
        let first = service_waiters_it
            .next()
            .expect("we should have at least one request");
        let context = first.context;
        txs.push(first.sender);
        let SubgraphRequest {
            subgraph_request, ..
        } = first.sg_request;
        let operation_name = subgraph_request
            .body()
            .operation_name
            .clone()
            .unwrap_or_default();

        let (parts, _) = subgraph_request.into_parts();
        let body =
            serde_json::to_string(&first.gql_request).expect("JSON serialization should not fail");
        let mut bytes = BytesMut::new();
        bytes.put_u8(b'[');
        bytes.extend_from_slice(&hyper::body::to_bytes(body).await?);
        for waiter in service_waiters_it {
            txs.push(waiter.sender);
            bytes.put(&b", "[..]);
            let body = serde_json::to_string(&waiter.gql_request)
                .expect("JSON serialization should not fail");
            bytes.extend_from_slice(&hyper::body::to_bytes(body).await?);
        }
        bytes.put_u8(b']');
        let body_bytes = bytes.freeze();
        // Reverse txs to get them in the right order
        txs.reverse();
        let request = http::Request::from_parts(parts, Body::from(body_bytes));
        Ok((operation_name, context, request, txs))
    }
}

// If a Batch is dropped and it still contains waiters, it's important to notify those waiters that
// their calls have failed.
//
// TODO: Figure out the implications, but panic for now if waiters is not empty
impl Drop for Batch {
    fn drop(&mut self) {
        if !self.waiters.is_empty() {
            panic!("TODO: waiters must be empty when a Batch is dropped");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::graphql;
    use crate::services::{SubgraphRequest, SubgraphResponse};
    use crate::Context;

    use hyper::body::to_bytes;
    use tokio::sync::oneshot;

    use super::Waiter;

    #[tokio::test(flavor = "multi_thread")]
    async fn it_assembles_batch() {
        let context = Context::new();

        // Assemble a list of waiters for testing
        let (receivers, waiters): (Vec<_>, Vec<_>) = (0..2)
            .map(|index| {
                let (tx, rx) = oneshot::channel();
                let graphql_request = graphql::Request::fake_builder()
                    .operation_name("batch_test")
                    .query(format!("query batch_test {{ slot{index} }}"))
                    .build();

                (
                    rx,
                    Waiter::new(
                        SubgraphRequest::fake_builder()
                            .subgraph_request(
                                http::Request::builder()
                                    .body(graphql_request.clone())
                                    .unwrap(),
                            )
                            .subgraph_name(format!("slot{index}"))
                            .build(),
                        graphql_request,
                        context.clone(),
                        tx,
                    ),
                )
            })
            .unzip();

        // Try to assemble them
        let (op_name, _context, request, txs) = Waiter::assemble_batch(waiters).await.unwrap();

        // Make sure we've assembled the request correctly
        assert_eq!(op_name, "batch_test");

        // We should see the aggregation of all of the requests
        let actual: Vec<graphql::Request> = serde_json::from_str(
            &String::from_utf8(to_bytes(request.into_body()).await.unwrap().to_vec()).unwrap(),
        )
        .unwrap();

        let expected: Vec<_> = (0..2)
            .map(|index| {
                graphql::Request::fake_builder()
                    .operation_name("batch_test")
                    .query(format!("query batch_test {{ slot{index} }}"))
                    .build()
            })
            .collect();
        assert_eq!(actual, expected);

        // We should also have all of the correct senders and they should be linked to the correct waiter
        // Note: We reverse the senders since they should be in reverse order when assembled
        assert_eq!(txs.len(), receivers.len());
        for (index, (tx, rx)) in Iterator::zip(txs.into_iter().rev(), receivers).enumerate() {
            let data = serde_json_bytes::json!({
                "data": {
                    format!("slot{index}"): "valid"
                }
            });
            let response = SubgraphResponse {
                response: http::Response::builder()
                    .body(graphql::Response::builder().data(data.clone()).build())
                    .unwrap(),
                context: Context::new(),
            };

            tx.send(Ok(response)).unwrap();

            // We want to make sure that we don't hang the test if we don't get the correct message
            let received = tokio::time::timeout(Duration::from_millis(10), rx)
                .await
                .unwrap()
                .unwrap()
                .unwrap();

            assert_eq!(received.response.into_body().data, Some(data));
        }
    }
}
