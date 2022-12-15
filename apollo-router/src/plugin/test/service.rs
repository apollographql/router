#![allow(dead_code, unreachable_pub)]
#![allow(missing_docs)] // FIXME

use std::panic;
use std::sync::Arc;
use std::thread;

use futures::channel::mpsc;
use futures::channel::oneshot;
use futures::future::BoxFuture;
use futures::lock::Mutex;
use futures::SinkExt;
use futures::StreamExt;
use tower::Service;

use crate::ExecutionRequest;
use crate::ExecutionResponse;
use crate::SubgraphRequest;
use crate::SubgraphResponse;
use crate::SupergraphRequest;
use crate::SupergraphResponse;

/// Build a mock service handler for the router pipeline.
macro_rules! mock_service {
    ($name:ident, $request_type:ty, $response_type:ty) => {
        paste::item! {
            mockall::mock! {
                #[derive(Debug)]
                #[allow(dead_code)]
                pub [<$name Service>] {
                    pub fn call(&mut self, req: $request_type) -> Result<$response_type, tower::BoxError>;
                }

                #[allow(dead_code)]
                impl Clone for [<$name Service>] {
                    fn clone(&self) -> [<Mock $name Service>];
                }
            }

            // mockall does not handle well the lifetime on Context
            impl tower::Service<$request_type> for [<Mock $name Service>] {
                type Response = $response_type;
                type Error = tower::BoxError;
                type Future = futures::future::BoxFuture<'static, Result<Self::Response, Self::Error>>;

                fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), tower::BoxError>> {
                    std::task::Poll::Ready(Ok(()))
                }
                #[track_caller]
                fn call(&mut self, req: $request_type) -> Self::Future {
                    let r  = self.call(req);
                    Box::pin(async move { r })
                }
            }
        }
    };
}

mock_service!(Supergraph, SupergraphRequest, SupergraphResponse);
mock_service!(Execution, ExecutionRequest, ExecutionResponse);
mock_service!(Subgraph, SubgraphRequest, SubgraphResponse);

pub(crate) struct MockService<Request, Response, Error>
where
    Request: Send,
    Response: Send,
    Error: Send,
{
    tx: mpsc::Sender<(
        Request,
        oneshot::Sender<thread::Result<Result<Response, Error>>>,
    )>,
}

impl<Request, Response, Error> Clone for MockService<Request, Response, Error>
where
    Request: Send,
    Response: Send,
    Error: Send,
{
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

impl<Request, Response, Error> MockService<Request, Response, Error>
where
    Request: Send + 'static,
    Response: Send + 'static,
    Error: Send + 'static,
{
    pub(crate) fn create<F>(mut closure: F) -> Self
    where
        F: FnMut(Request) -> Result<Response, Error> + Send + 'static,
    {
        let (tx, mut rx) = mpsc::channel::<(
            Request,
            oneshot::Sender<thread::Result<Result<Response, Error>>>,
        )>(100);

        let store_sender = Arc::new(Mutex::new(None));

        tokio::task::spawn(async move {
            let store = store_sender.clone();
            if let Err(e) = tokio::task::spawn(async move {
                while let Some((request, sender)) = rx.next().await {
                    *store.lock().await = Some(sender);
                    let res = closure(request);
                    let sender = store.lock().await.take().unwrap();
                    sender.send(Ok(res));
                }
                //println!("end of loop");
            })
            .await
            {
                let error = e.try_into_panic().unwrap();
                println!("task got panic: {:?}", error);
                let sender = store_sender.lock().await.take().unwrap();
                sender.send(Err(error));
            }
            //println!("end of outer task");
        });

        Self { tx }
    }
}

impl<Request, Response, Error> Service<Request> for MockService<Request, Response, Error>
where
    Request: Send + 'static,
    Response: Send + 'static,
    Error: Send + 'static,
{
    type Response = Response;

    type Error = Error;

    type Future = BoxFuture<'static, Result<Response, Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    #[track_caller]
    fn call(&mut self, request: Request) -> Self::Future {
        let (sender, receiver) = oneshot::channel();

        let mut tx = self.tx.clone();

        Box::pin(async move {
            tx.send((request, sender))
                .await
                .expect("mock service task closed");

            receiver
                .await
                .expect("oneshot sender dropped")
                .expect("mock service panicked")
        })
    }
}
