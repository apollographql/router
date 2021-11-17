use super::FederatedServerError;
use crate::configuration::{Configuration, ListenAddr};
use apollo_router_core::prelude::*;
use derivative::Derivative;
use futures::channel::oneshot;
use futures::prelude::*;
#[cfg(test)]
use mockall::{automock, predicate::*};
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

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
        listener: Option<Box<dyn Listener>>,
    ) -> Pin<Box<dyn Future<Output = Result<HttpServerHandle, FederatedServerError>> + Send>>
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
    server_future:
        Pin<Box<dyn Future<Output = Result<Box<dyn Listener>, FederatedServerError>> + Send>>,

    /// The listen address that the server is actually listening on.
    /// If the socket address specified port zero the OS will assign a random free port.
    #[allow(dead_code)]
    listen_address: ListenAddr,
}

impl HttpServerHandle {
    pub(crate) fn new(
        shutdown_sender: oneshot::Sender<()>,
        server_future: Pin<
            Box<dyn Future<Output = Result<Box<dyn Listener>, FederatedServerError>> + Send>,
        >,
        listen_address: ListenAddr,
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

    pub(crate) async fn restart<Fetcher, ServerFactory>(
        self,
        factory: &ServerFactory,
        graph: Arc<Fetcher>,
        configuration: Arc<Configuration>,
    ) -> Result<Self, FederatedServerError>
    where
        Fetcher: graphql::Fetcher + 'static,
        ServerFactory: HttpServerFactory,
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
        tracing::info!("previous server is closed");

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
            .create(Arc::clone(&graph), Arc::clone(&configuration), listener)
            .await?;
        tracing::debug!("Restarted on {}", handle.listen_address());

        Ok(handle)
    }

    pub(crate) fn listen_address(&self) -> &ListenAddr {
        &self.listen_address
    }
}

pub(crate) trait Listener: Send + Unpin {
    fn accept<'a>(
        &'a self,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = io::Result<(
                        Box<dyn AsyncReadWrite + Send + Unpin + 'static>,
                        ListenAddr,
                    )>,
                > + Send
                + 'a,
        >,
    >;
    fn local_addr(&self) -> io::Result<ListenAddr>;
}

impl Listener for TcpListener {
    fn accept<'a>(
        &'a self,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = io::Result<(
                        Box<dyn AsyncReadWrite + Send + Unpin + 'static>,
                        ListenAddr,
                    )>,
                > + Send
                + 'a,
        >,
    > {
        self.accept()
            .map(|res| {
                let (stream, addr) = res?;
                Ok((Box::new(stream) as Box<_>, addr.into()))
            })
            .boxed()
    }
    fn local_addr(&self) -> io::Result<ListenAddr> {
        self.local_addr().map(Into::into)
    }
}

#[cfg(unix)]
impl Listener for UnixListener {
    fn accept<'a>(
        &'a self,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = io::Result<(
                        Box<dyn AsyncReadWrite + Send + Unpin + 'static>,
                        ListenAddr,
                    )>,
                > + Send
                + 'a,
        >,
    > {
        self.accept()
            .map(|res| {
                let (stream, addr) = res?;
                Ok((Box::new(stream) as Box<_>, addr.into()))
            })
            .boxed()
    }
    fn local_addr(&self) -> io::Result<ListenAddr> {
        self.local_addr().map(Into::into)
    }
}

pub(crate) trait AsyncReadWrite: tokio::io::AsyncRead + tokio::io::AsyncWrite {
    fn set_nodelay(&self, nodelay: bool) -> io::Result<()>;
}

impl AsyncReadWrite for TcpStream {
    fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        self.set_nodelay(nodelay)
    }
}

#[cfg(unix)]
impl AsyncReadWrite for UnixStream {
    fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::channel::oneshot;
    use std::net::SocketAddr;
    use std::str::FromStr;
    use test_env_log::test;

    #[test(tokio::test)]
    async fn sanity() {
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let listener = Box::new(TcpListener::bind("127.0.0.1:0").await.unwrap()) as Box<_>;

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
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        // TODO get path from tempfile
        let listener = Box::new(UnixListener::bind("/tmp/sanity_unix.sock").unwrap()) as Box<_>;

        HttpServerHandle::new(
            shutdown_sender,
            futures::future::ready(Ok(listener)).boxed(),
            ListenAddr::UnixSocket("/tmp/sanity_unix.sock".into()),
        )
        .shutdown()
        .await
        .expect("Should have waited for shutdown");

        shutdown_receiver
            .await
            .expect("Should have been send notification to shutdown");
    }
}
