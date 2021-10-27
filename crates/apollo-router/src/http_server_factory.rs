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
    shutdown_sender: tokio::sync::watch::Sender<bool>,

    /// Future to wait on for graceful shutdown
    #[derivative(Debug = "ignore")]
    server_future: Pin<Box<dyn Future<Output = Result<(), FederatedServerError>> + Send>>,

    /// The listen address that the server is actually listening on.
    /// If the socket address specified port zero the OS will assign a random free port.
    #[allow(dead_code)]
    listen_address: SocketAddr,

    return_listener: ReturnListener,
}

impl HttpServerHandle {
    pub(crate) fn new(
        shutdown_sender: tokio::sync::watch::Sender<bool>,
        server_future: Pin<Box<dyn Future<Output = Result<(), FederatedServerError>> + Send>>,
        listen_address: SocketAddr,
        return_listener: ReturnListener,
    ) -> Self {
        Self {
            shutdown_sender,
            server_future,
            listen_address,
            return_listener,
        }
    }

    pub(crate) async fn shutdown(self) -> Result<(), FederatedServerError> {
        if let Err(_err) = self.shutdown_sender.send(true) {
            tracing::error!("Failed to notify http thread of shutdown")
        };
        let _listener = self.return_listener.stop().await;
        self.server_future.await
    }

    pub(crate) async fn return_listener(self) -> Option<TcpListener> {
        if let Err(_err) = self.shutdown_sender.send(true) {
            tracing::error!("Failed to notify http thread of shutdown")
        };
        let listener = self.return_listener.stop().await;
        tokio::task::spawn(self.server_future.inspect(|_| {
            tracing::info!("previous server is closed");
        }));

        listener
    }

    pub(crate) fn listen_address(&self) -> SocketAddr {
        self.listen_address
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
    pub(crate) async fn stop(self) -> Option<TcpListener> {
        if self.stop_listen_tx.send(()).is_err() {
            tracing::error!("cannot return listener, the server task was canceled");
            return None;
        }

        match self.listener_rx.await {
            Err(oneshot::Canceled) => {
                tracing::error!("cannot return listener, the server task was canceled");
                None
            }
            Ok(listener) => Some(listener),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use test_env_log::test;

    #[test(tokio::test)]
    async fn sanity() {
        let (shutdown_sender, mut shutdown_receiver) = tokio::sync::watch::channel(false);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (return_listener, _stop_listen_rx, listen_tx) = ReturnListener::new();

        let _ = listen_tx.send(listener);
        HttpServerHandle {
            listen_address: SocketAddr::from_str("127.0.0.1:0").unwrap(),
            shutdown_sender,
            server_future: futures::future::ready(Ok(())).boxed(),
            return_listener,
        }
        .shutdown()
        .await
        .expect("Should have waited for shutdown");

        shutdown_receiver
            .changed()
            .await
            .expect("Should have been send notification to shutdown");
    }
}
