//! An assembly of utility functions and core structures used to implement batching support within
//! the router.
//!
//! In addition to the core batching functionality, as expressed in `BatchQuery` and
//! `Batch`, there are a series of utility functions for efficiently converting
//! graphql Requests to/from batch representation in a variety of formats: JSON, bytes

use std::fmt;

use bytes::BufMut;
use bytes::BytesMut;
use hyper::Body;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tower::BoxError;

use crate::error::FetchError;
use crate::graphql;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::Context;

#[derive(Clone, Debug)]
pub(crate) struct BatchQuery {
    /// The index of this query relative to the entire batch
    index: usize,

    /// A channel sender for sending updates to the entire batch
    sender: mpsc::Sender<BatchHandlerMessage>,
}

impl fmt::Display for BatchQuery {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // write!(f, "index: {}, ", self.index)?;
        // // Use try_lock. If the shared batch is locked, we won't display it.
        // // TODO: Maybe improve to handle the error...?
        // let guard = self.shared.try_lock().ok_or(fmt::Error)?;
        // write!(f, "size: {}, ", guard.size)?;
        // write!(f, "expected: {:?}, ", guard.expected)?;
        // write!(f, "seen: {:?}", guard.seen)?;
        // for (service, waiters) in guard.waiters.iter() {
        //     write!(f, ", service: {}, waiters: {}", service, waiters.len())?;
        // }
        // Ok(())
        todo!()
    }
}

impl BatchQuery {
    /// Inform the batch of the amount of fetches needed for this element of the batch query
    pub(crate) async fn set_subgraph_fetches(&self, fetches: usize) {
        // TODO: How should we handle the sender dying?
        self.sender
            .send(BatchHandlerMessage::UpdateFetchCountForIndex {
                index: self.index,
                count: fetches,
            })
            .await
            .unwrap();
    }

    /// Signal to the batch handler that this specific batch query is ready to execute.
    ///
    /// The returned channel can be awaited to receive the GraphQL response, when ready.
    pub(crate) async fn signal_ready(
        &self,
        request: SubgraphRequest,
    ) -> oneshot::Receiver<Result<SubgraphResponse, BoxError>> {
        // Create a receiver for this query so that it can eventually get the request meant for it
        let (tx, rx) = oneshot::channel();

        // TODO: How should we handle the sender dying?
        self.sender
            .send(BatchHandlerMessage::SignalReady {
                index: self.index,
                request,
                response_sender: tx,
            })
            .await
            .unwrap();

        rx
    }
}

enum BatchHandlerMessage {
    /// Abort one of the sub requests
    // TODO: How do we know which of the subfetches of the entire query to abort? Is it all of them?
    Abort { index: usize, reason: String },

    /// A query has reached the subgraph service and is ready to execute
    SignalReady {
        index: usize,
        request: SubgraphRequest,
        response_sender: oneshot::Sender<Result<SubgraphResponse, BoxError>>,
    },

    /// A query has passed query planning and now knows how many fetches are needed
    /// to complete.
    UpdateFetchCountForIndex { index: usize, count: usize },
}

#[derive(Debug)]
pub(crate) struct Batch {
    /// A sender channel to communicate with the batching handler
    spawn_tx: mpsc::Sender<BatchHandlerMessage>,

    /// The spawned batching handler task handle
    ///
    /// Note: This _must_ be aborted or else the spawned task will run forever
    /// with no way to cancel it.
    spawn_handle: JoinHandle<()>,
}

impl Batch {
    /// Creates a new batch, spawning an async task for handling updates to the
    /// batch lifecycle.
    pub(crate) fn spawn_handler(size: usize) -> Self {
        // Create the message channel pair for sending update events to the spawned task
        // TODO: Should the upper limit here be configurable?
        let (spawn_tx, rx) = mpsc::channel(100);
        let spawn_handle = tokio::spawn(async move {
            /// Helper struct for keeping track of the expected vs seen pairs for a batch query
            ///
            /// Note: We also keep track of whether a fetch has been aborted so that later queries can quickly
            /// short-circuit if needed.
            #[derive(Clone)]
            struct Fetches {
                needed: usize,
                seen: usize,

                aborted: Option<String>,
            }
            impl Fetches {
                fn is_ready(&self) -> bool {
                    self.needed == self.seen
                }
            }

            // We need to keep track of how many sub fetches are needed per query,
            // but that information won't be known until each portion of the entire
            // batch passes query planning.
            // TODO: Is there a more efficient data structure for keeping track of all of these counts?
            let mut fetches_per_query: Vec<Option<Fetches>> = vec![None; size];

            // We also need to keep track of all requests we need to make and their send handles
            let mut requests: Vec<
                Vec<(
                    SubgraphRequest,
                    oneshot::Sender<Result<SubgraphResponse, BoxError>>,
                )>,
            > = Vec::from_iter((0..size).map(|_| Vec::new()));

            // Start handling messages from various portions of the request lifecycle
            // TODO: Do we want to panic if all of the senders have dropped?
            while let Some(msg) = rx.recv().await {
                match msg {
                    // Just clear out the fetch and error out the requests
                    BatchHandlerMessage::Abort { index, reason } => {
                        // Get all of the current waiting requests and short-circuit them
                        if let Some(fetches) = fetches_per_query[index] {
                            // Send back an error to every waiting request
                            let send_error: Result<Vec<_>, _> = requests[index]
                                .iter()
                                .map(|(request, sender)| {
                                    sender.send(Err(Box::new(
                                        FetchError::SubrequestBatchingError {
                                            // TODO: How should we get this? The field subgraph_name seems wrong
                                            service: request.subgraph_name.unwrap(),
                                            reason: format!("request aborted: {reason}"),
                                        },
                                    )))
                                })
                                .collect();

                            // TODO: How should we handle send errors?
                            send_error.unwrap();

                            // Nuke this query of requests
                            requests[index].clear();
                        }

                        // Mark the index as being aborted for future requests
                        fetches_per_query[index] = Some(Fetches {
                            needed: 0,
                            seen: 0,
                            aborted: Some(reason),
                        });
                    }

                    // TODO: Do we want to handle if a query is outside of the range of the batch size?
                    BatchHandlerMessage::UpdateFetchCountForIndex { index, count } => {
                        fetches_per_query[index] = Some(Fetches {
                            needed: count,
                            seen: 0,
                            aborted: None,
                        });
                    }

                    // TODO: Do we want to handle if a query is outside of the range of the batch size?
                    BatchHandlerMessage::SignalReady {
                        index,
                        request,
                        response_sender,
                    } => {
                        // If we got a message that a subfetch from an aborted request has arrived, just short it out now
                        // TODO: How do we handle the fetch not being present?
                        if let Some(reason) = fetches_per_query[index].unwrap().aborted {
                            // TODO: How should we handle send failure here?
                            response_sender
                                .send(Err(Box::new(FetchError::SubrequestBatchingError {
                                    // TODO: How should we get this? The field subgraph_name seems wrong
                                    service: request.subgraph_name.unwrap(),
                                    reason: format!("request aborted: {reason}"),
                                })))
                                .unwrap();
                        } else {
                            requests[index].push((request, response_sender));
                        }
                    }
                }

                // If all of the fetches are ready, then we can start actually making the batched request
                if fetches_per_query
                    .iter()
                    .all(|f| f.is_some() && f.unwrap().is_ready())
                {
                    break;
                }
            }

            todo!("implement the fetching logic");
        });

        Self {
            spawn_tx,
            spawn_handle,
        }
    }

    /// Create a batch query for a specific index in this batch
    // TODO: Do we want to panic / error if the index is out of range?
    pub(crate) fn query_for_index(&self, index: usize) -> BatchQuery {
        BatchQuery {
            index,
            sender: self.spawn_tx.clone(),
        }
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
// impl Drop for Batch {
//     fn drop(&mut self) {
//         if !self.waiters.is_empty() {
//             panic!("TODO: waiters must be empty when a Batch is dropped");
//         }
//     }
// }

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hyper::body::to_bytes;
    use tokio::sync::oneshot;

    use super::Waiter;
    use crate::graphql;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;
    use crate::Context;

    // Possible example test
    // #[tokio::test(flavor = "multi_thread")]
    // async fn it_assembles_batch() {
    //     // For this test we'll create a mock batch where each subrequest makes
    //     // 2 times its index in fetches.
    //     const BATCH_SIZE: usize = 4;

    //     let batch = Batch::spawn_handler(BATCH_SIZE);
    //     let queries: Vec<_> = (0..BATCH_SIZE)
    //         .map(|index| batch.query_for_index(index))
    //         .collect();

    //     // Notify the handler about the needed fetch count for each subrequest
    //     for (index, query) in queries.iter().enumerate() {
    //         query.set_subgraph_fetches(index * 2);
    //     }

    //     // Add some delay to simulate the time between router stages
    //     tokio::time::sleep(Duration::from_millis(200)).await;

    //     // Notify that each query is ready
    //     let receivers =
    //         futures::future::join_all(queries.iter().map(BatchQuery::signal_ready)).await;

    //     // Make sure that we get back the correct responses for them all
    //     for (index, rx) in receivers.into_iter().enumerate() {
    //         let result = rx.await.unwrap().unwrap();

    //         assert_eq!(
    //             result.response.body().data,
    //             Some(serde_json_bytes::Value::String(
    //                 format!("{index}: {}", index * 2).into()
    //             ))
    //         );
    //     }
    // }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_assembles_batch() {
        let context = Context::new();

        // Assemble a list of waiters for testing
        let (receivers, waiters): (Vec<_>, Vec<_>) = (0..2)
            .map(|index| {
                let (tx, rx) = oneshot::channel();
                let graphql_request = graphql::Request::fake_builder()
                    .operation_name(format!("batch_test_{index}"))
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

        // Make sure that the name of the entire batch is that of the first
        assert_eq!(op_name, "batch_test_0");

        // We should see the aggregation of all of the requests
        let actual: Vec<graphql::Request> = serde_json::from_str(
            &String::from_utf8(to_bytes(request.into_body()).await.unwrap().to_vec()).unwrap(),
        )
        .unwrap();

        let expected: Vec<_> = (0..2)
            .map(|index| {
                graphql::Request::fake_builder()
                    .operation_name(format!("batch_test_{index}"))
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
