use super::FederatedServerError;
use crate::configuration::Configuration;
use apollo_router_core::prelude::*;
use derivative::Derivative;
use futures::channel::oneshot;
use futures::prelude::*;
#[cfg(test)]
use mockall::{automock, predicate::*};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use tokio::net::TcpListener;

/// Factory for creating the http server component.
///
/// This trait enables us to test that `StateMachine` correctly recreates the http server when
/// necessary e.g. when listen address changes.
#[cfg_attr(test, automock)]
pub(crate) trait HttpServerFactory {
    fn create<F>(
        &self,
        graph: Arc<F>,
        configuration: Arc<Configuration>,
        listener: Option<TcpListener>,
    ) -> Pin<Box<dyn Future<Output = HttpServerHandle> + Send>>
    where
        F: graphql::Fetcher + 'static;
}

/// A handle with with a client can shut down the server gracefully.
/// This relies on the underlying server implementation doing the right thing.
/// There are various ways that a user could prevent this working, including holding open connections
/// and sending huge requests. There is potential work needed for hardening.
#[derive(Derivative)]
#[derivative(Debug)]
pub(crate) struct HttpServerHandle {
    /// Sender to use to notify of shutdown
    pub(crate) shutdown_sender: oneshot::Sender<()>,

    /// Future to wait on for graceful shutdown
    #[derivative(Debug = "ignore")]
    pub(crate) server_future:
        Pin<Box<dyn Future<Output = Result<(), FederatedServerError>> + Send>>,

    /// The listen address that the server is actually listening on.
    /// If the socket address specified port zero the OS will assign a random free port.
    #[allow(dead_code)]
    pub(crate) listen_address: SocketAddr,

    pub(crate) return_listener: ReturnListener,
}

impl HttpServerHandle {
    pub(crate) async fn shutdown(self) -> Result<(), FederatedServerError> {
        if let Err(_err) = self.shutdown_sender.send(()) {
            tracing::error!("Failed to notify http thread of shutdown")
        };
        self.server_future.await
    }
}

#[derive(Debug)]
pub(crate) struct ReturnListener {
    stop_listen_tx: oneshot::Sender<()>,
    listener_rx: oneshot::Receiver<TcpListener>,
}

impl ReturnListener {
    pub fn new() -> (
        ReturnListener,
        oneshot::Receiver<()>,
        oneshot::Sender<TcpListener>,
    ) {
        let (stop_listen_tx, stop_listen_rx) = oneshot::channel::<()>();
        let (listener_tx, listener_rx) = oneshot::channel::<TcpListener>();

        (
            ReturnListener {
                stop_listen_tx,
                listener_rx,
            },
            stop_listen_rx,
            listener_tx,
        )
    }

    ///asks the running server to give back the listener socket
    pub(crate) async fn stop(self) -> TcpListener {
        self.stop_listen_tx.send(()).unwrap();
        self.listener_rx.await.unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use test_env_log::test;

    #[test(tokio::test)]
    async fn sanity() {
        let (shutdown_sender, shutdown_receiver) = futures::channel::oneshot::channel();
        HttpServerHandle {
            listen_address: SocketAddr::from_str("127.0.0.1:0").unwrap(),
            shutdown_sender,
            server_future: futures::future::ready(Ok(())).boxed(),
        }
        .shutdown()
        .await
        .expect("Should have waited for shutdown");

        shutdown_receiver
            .into_future()
            .await
            .expect("Should have been send notification to shutdown");
    }
}
