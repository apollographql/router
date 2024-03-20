//! An assembly of utility functions and core structures used to implement batching support within
//! the router.
//!
//! In addition to the core batching functionality, as expressed in `BatchQuery` and
//! `Batch`, there are a series of utility functions for efficiently converting
//! graphql Requests to/from batch representation in a variety of formats: JSON, bytes

use std::fmt;

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

/// A query that is part of a batch.
///
/// Note: We do NOT want this to implement `Clone` because it holds a sender
/// to the batch, which is waiting for all batch queries to drop their senders
/// in order to finish processing the batch.
#[derive(Debug)]
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
        self,
        request: SubgraphRequest,
        gql_request: graphql::Request,
    ) -> oneshot::Receiver<Result<SubgraphResponse, BoxError>> {
        // Create a receiver for this query so that it can eventually get the request meant for it
        let (tx, rx) = oneshot::channel();

        // TODO: How should we handle the sender dying?
        self.sender
            .send(BatchHandlerMessage::SignalReady {
                index: self.index,
                request,
                gql_request,
                response_sender: tx,
            })
            .await
            .unwrap();

        rx
    }
}

#[derive(Debug)]
enum BatchHandlerMessage {
    /// Abort one of the sub requests
    // TODO: How do we know which of the subfetches of the entire query to abort? Is it all of them?
    Abort { index: usize, reason: String },

    /// A query has reached the subgraph service and is ready to execute
    SignalReady {
        index: usize,
        request: SubgraphRequest,
        gql_request: graphql::Request,
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
        tracing::debug!("New batch created with size {size}");

        // Create the message channel pair for sending update events to the spawned task
        // TODO: Should the upper limit here be configurable?
        let (spawn_tx, mut rx) = mpsc::channel(100);
        let spawn_handle = tokio::spawn(async move {
            /// Helper struct for keeping track of the expected vs seen pairs for a batch query
            ///
            /// Note: We also keep track of whether a fetch has been aborted so that later queries can quickly
            /// short-circuit if needed.
            #[derive(Debug, Clone)]
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
                    graphql::Request,
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
                        if fetches_per_query[index].is_some() {
                            // Clear out the requests for this index and grab the old ones
                            let pending_requests = std::mem::take(&mut requests[index]);

                            let send_error: Result<Vec<_>, _> = pending_requests
                                .into_iter()
                                .map(|(request, _, sender)| {
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
                        tracing::debug!("Updating fetch count for index {index} with {count}");

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
                        gql_request,
                        response_sender,
                    } => {
                        tracing::debug!("Query at {index} is ready for processing");

                        // Get the fetch entry.
                        // Note: We can panic here because an out-of-bounds index or a missing entry is a serious programming error
                        let fetches = fetches_per_query
                            .get_mut(index)
                            .expect("batch query has an index outside of the batches range")
                            .as_mut()
                            .expect("batch query was signaled ready before knowing its count");

                        // If we got a message that a subfetch from an aborted request has arrived, just short it out now
                        // TODO: How do we handle the fetch not being present?
                        if let Some(reason) = fetches.aborted.as_ref() {
                            // TODO: How should we handle send failure here?
                            response_sender
                                .send(Err(Box::new(FetchError::SubrequestBatchingError {
                                    // TODO: How should we get this? The field subgraph_name seems wrong
                                    service: request.subgraph_name.unwrap(),
                                    reason: format!("request aborted: {reason}"),
                                })))
                                .unwrap();
                        } else {
                            // Add to our count of seen sub queries and keep track of the channel for
                            // responding to the request later.
                            fetches.seen += 1;
                            requests[index].push((request, gql_request, response_sender));
                        }
                    }
                }
            }

            // Make sure that we are actually ready and haven't forgotten to update something somewhere
            if fetches_per_query
                .iter()
                .any(|f| !f.as_ref().is_some_and(Fetches::is_ready))
            {
                tracing::error!("All senders for the batch have dropped before reaching the ready state: {fetches_per_query:#?}");
                panic!("all senders for the batch have dropped before reaching the ready state");
            }

            // TODO: Do we want to generate a UUID for a batch for observability reasons?
            tracing::debug!("Assembling {size} requests into a batch");
            for request in requests {
                // TODO: Do we need the context?
                // TODO: How do we handle an error from assembling the batch?
                let request = assemble_batch(todo!(), request).await;
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

impl Drop for Batch {
    fn drop(&mut self) {
        // Make sure that we kill the background task if the batch itself is dropped
        self.spawn_handle.abort();
    }
}

// Assemble a single batch request to a subgraph
async fn assemble_batch(
    context: Context,
    requests: Vec<(
        SubgraphRequest,
        graphql::Request,
        oneshot::Sender<Result<SubgraphResponse, BoxError>>,
    )>,
) -> Result<
    (
        String,
        Context,
        http::Request<Body>,
        Vec<oneshot::Sender<Result<SubgraphResponse, BoxError>>>,
    ),
    BoxError,
> {
    // Extract the collection of parts from the requests
    let (txs, request_pairs): (Vec<_>, Vec<_>) = requests
        .into_iter()
        .map(|(request, gql_request, sender)| (sender, (request, gql_request)))
        .unzip();
    let (requests, gql_requests): (Vec<_>, Vec<_>) = request_pairs.into_iter().unzip();

    // Construct the actual byte body of the batched request
    let bytes = hyper::body::to_bytes(
        serde_json::to_string(&gql_requests).expect("JSON serialization should not fail"),
    )
    .await?;

    // Grab the common info from the first request
    let first_request = requests
        .into_iter()
        .next()
        .expect("batch to assemble had no requests")
        .subgraph_request;
    let operation_name = first_request
        .body()
        .operation_name
        .clone()
        .unwrap_or_default();
    let (parts, _) = first_request.into_parts();

    // Generate the final request and pass it up
    // TODO: The previous implementation reversed the txs here. Is that necessary?
    let request = http::Request::from_parts(parts, Body::from(bytes));
    Ok((operation_name, context, request, txs))
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

    use crate::graphql;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;
    use crate::Context;

    use super::assemble_batch;

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

        // Assemble a list of requests for testing
        let (receivers, requests): (Vec<_>, Vec<_>) = (0..2)
            .map(|index| {
                let (tx, rx) = oneshot::channel();
                let graphql_request = graphql::Request::fake_builder()
                    .operation_name(format!("batch_test_{index}"))
                    .query(format!("query batch_test {{ slot{index} }}"))
                    .build();

                (
                    rx,
                    (
                        SubgraphRequest::fake_builder()
                            .subgraph_request(
                                http::Request::builder()
                                    .body(graphql_request.clone())
                                    .unwrap(),
                            )
                            .subgraph_name(format!("slot{index}"))
                            .build(),
                        graphql_request,
                        tx,
                    ),
                )
            })
            .unzip();

        // Try to assemble them
        let (op_name, _context, request, txs) = assemble_batch(context, requests).await.unwrap();

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
        for (index, (tx, rx)) in Iterator::zip(txs.into_iter(), receivers).enumerate() {
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
