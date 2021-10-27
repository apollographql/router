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
    shutdown_sender: oneshot::Sender<()>,

    /// Future to wait on for graceful shutdown
    #[derivative(Debug = "ignore")]
    server_future: Pin<Box<dyn Future<Output = Result<TcpListener, FederatedServerError>> + Send>>,

    /// The listen address that the server is actually listening on.
    /// If the socket address specified port zero the OS will assign a random free port.
    #[allow(dead_code)]
    listen_address: SocketAddr,
}

impl HttpServerHandle {
    pub(crate) fn new(
        shutdown_sender: oneshot::Sender<()>,
        server_future: Pin<
            Box<dyn Future<Output = Result<TcpListener, FederatedServerError>> + Send>,
        >,
        listen_address: SocketAddr,
    ) -> Self {
        Self {
            shutdown_sender,
            server_future,
            listen_address,
        }
    }

    pub(crate) async fn shutdown(self) -> Result<(), FederatedServerError> {
        if let Err(_err) = self.shutdown_sender.send(()) {
            tracing::error!("Failed to notify http thread of shutdown")
        };
        self.server_future.await.map(|_| ())
    }

    pub(crate) async fn return_listener(self) -> Result<TcpListener, FederatedServerError> {
        if let Err(_err) = self.shutdown_sender.send(()) {
            tracing::error!("Failed to notify http thread of shutdown")
        };

        let listener = self.server_future.await;
        tracing::info!("previous server is closed");

        listener
    }

    pub(crate) fn listen_address(&self) -> SocketAddr {
        self.listen_address
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::channel::oneshot;
    use std::str::FromStr;
    use test_env_log::test;

    #[test(tokio::test)]
    async fn sanity() {
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();

        HttpServerHandle::new(
            shutdown_sender,
            futures::future::ready(Ok(listener)).boxed(),
            SocketAddr::from_str("127.0.0.1:0").unwrap(),
        )
        .shutdown()
        .await
        .expect("Should have waited for shutdown");

        shutdown_receiver
            .await
            .expect("Should have been send notification to shutdown");
    }
}
