use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use derivative::Derivative;
use futures::channel::oneshot;
use futures::prelude::*;

use super::router::ApolloRouterError;
use crate::configuration::Configuration;
use crate::configuration::ListenAddr;
use crate::plugin::Handler;
use crate::router_factory::RouterServiceFactory;

/// Factory for creating the http server component.
///
/// This trait enables us to test that `StateMachine` correctly recreates the http server when
/// necessary e.g. when listen address changes.
pub(crate) trait HttpServerFactory {
    type Future: Future<Output = Result<HttpServerHandle, ApolloRouterError>> + Send;

    fn create<RF>(
        &self,
        service_factory: RF,
        configuration: Arc<Configuration>,
        listener: Option<Listener>,
        plugin_handlers: HashMap<String, Handler>,
    ) -> Self::Future
    where
        RF: RouterServiceFactory;
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
    server_future: Pin<Box<dyn Future<Output = Result<Listener, ApolloRouterError>> + Send>>,

    /// The listen address that the server is actually listening on.
    /// If the socket address specified port zero the OS will assign a random free port.
    listen_address: ListenAddr,
}

impl HttpServerHandle {
    pub(crate) fn new(
        shutdown_sender: oneshot::Sender<()>,
        server_future: Pin<Box<dyn Future<Output = Result<Listener, ApolloRouterError>> + Send>>,
        listen_address: ListenAddr,
    ) -> Self {
        Self {
            shutdown_sender,
            server_future,
            listen_address,
        }
    }

    pub(crate) async fn shutdown(self) -> Result<(), ApolloRouterError> {
        if let Err(_err) = self.shutdown_sender.send(()) {
            tracing::error!("Failed to notify http thread of shutdown")
        };
        let _listener = self.server_future.await?;
        #[cfg(unix)]
        {
            if let ListenAddr::UnixSocket(path) = self.listen_address {
                let _ = tokio::fs::remove_file(path).await;
            }
        }
        Ok(())
    }

    pub(crate) async fn restart<RF, SF>(
        self,
        factory: &SF,
        router: RF,
        configuration: Arc<Configuration>,
        plugin_handlers: HashMap<String, Handler>,
    ) -> Result<Self, ApolloRouterError>
    where
        SF: HttpServerFactory,
        RF: RouterServiceFactory,
    {
        // we tell the currently running server to stop
        if let Err(_err) = self.shutdown_sender.send(()) {
            tracing::error!("Failed to notify http thread of shutdown")
        };

        // when the server receives the shutdown signal, it stops accepting new
        // connections, and returns the TCP listener, to reuse it in the next server
        // it is necessary to keep the queue of new TCP sockets associated with
        // the listener instead of dropping them
        let listener = self.server_future.await;
        tracing::debug!("previous server stopped");

        // we keep the TCP listener if it is compatible with the new configuration
        let listener = if self.listen_address != configuration.server.listen {
            None
        } else {
            match listener {
                Ok(listener) => Some(listener),
                Err(e) => {
                    tracing::error!("the previous listen socket failed: {}", e);
                    None
                }
            }
        };

        let handle = factory
            .create(
                router,
                Arc::clone(&configuration),
                listener,
                plugin_handlers,
            )
            .await?;
        tracing::debug!("restarted on {}", handle.listen_address());

        Ok(handle)
    }

    pub(crate) fn listen_address(&self) -> &ListenAddr {
        &self.listen_address
    }
}

pub(crate) enum Listener {
    Tcp(tokio::net::TcpListener),
    #[cfg(unix)]
    Unix(tokio::net::UnixListener),
}

pub(crate) enum NetworkStream {
    Tcp(tokio::net::TcpStream),
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
}

impl Listener {
    pub(crate) fn local_addr(&self) -> std::io::Result<ListenAddr> {
        match self {
            Listener::Tcp(listener) => listener.local_addr().map(Into::into),
            #[cfg(unix)]
            Listener::Unix(listener) => listener.local_addr().map(|addr| {
                ListenAddr::UnixSocket(
                    addr.as_pathname()
                        .map(ToOwned::to_owned)
                        .unwrap_or_default(),
                )
            }),
        }
    }

    pub(crate) async fn accept(&mut self) -> std::io::Result<NetworkStream> {
        match self {
            Listener::Tcp(listener) => listener
                .accept()
                .await
                .map(|(stream, _)| NetworkStream::Tcp(stream)),
            #[cfg(unix)]
            Listener::Unix(listener) => listener
                .accept()
                .await
                .map(|(stream, _)| NetworkStream::Unix(stream)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::str::FromStr;

    use futures::channel::oneshot;
    use test_log::test;

    use super::*;

    #[test(tokio::test)]
    async fn sanity() {
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let listener = Listener::Tcp(tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap());

        HttpServerHandle::new(
            shutdown_sender,
            futures::future::ready(Ok(listener)).boxed(),
            SocketAddr::from_str("127.0.0.1:0").unwrap().into(),
        )
        .shutdown()
        .await
        .expect("Should have waited for shutdown");

        shutdown_receiver
            .await
            .expect("Should have been send notification to shutdown");
    }

    #[test(tokio::test)]
    #[cfg(unix)]
    async fn sanity_unix() {
        let temp_dir = tempfile::tempdir().unwrap();
        let sock = temp_dir.as_ref().join("sock");
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let listener = Listener::Unix(tokio::net::UnixListener::bind(&sock).unwrap());

        HttpServerHandle::new(
            shutdown_sender,
            futures::future::ready(Ok(listener)).boxed(),
            ListenAddr::UnixSocket(sock),
        )
        .shutdown()
        .await
        .expect("Should have waited for shutdown");

        shutdown_receiver
            .await
            .expect("Should have been send notification to shutdown");
    }
}
