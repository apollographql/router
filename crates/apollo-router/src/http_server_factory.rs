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
use tokio_util::either::Either;

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
        listener: Option<AnyListener>,
    ) -> future::BoxFuture<'static, Result<HttpServerHandle, FederatedServerError>>
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
    server_future: future::BoxFuture<'static, Result<AnyListener, FederatedServerError>>,

    /// The listen address that the server is actually listening on.
    /// If the socket address specified port zero the OS will assign a random free port.
    #[allow(dead_code)]
    listen_address: AnyAddr,
}

impl HttpServerHandle {
    pub(crate) fn new(
        shutdown_sender: oneshot::Sender<()>,
        server_future: future::BoxFuture<'static, Result<AnyListener, FederatedServerError>>,
        listen_address: AnyAddr,
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
        #[allow(unused_variables)]
        let listener = self.server_future.await?;
        #[cfg(unix)]
        {
            let local_addr = listener.local_addr();
            if let Ok(AnyAddr::Right(unix_addr)) = local_addr {
                if let Some(path) = unix_addr.as_pathname() {
                    let _ = tokio::fs::remove_file(path).await;
                }
            }
        }
        Ok(())
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
        #[cfg(unix)]
        let listener = match (&self.listen_address, &configuration.server.listen) {
            (AnyAddr::Left(a), ListenAddr::SocketAddr(b)) if a == b => listener.ok(),
            (AnyAddr::Right(a), ListenAddr::UnixSocket(b)) if a.as_pathname() == Some(b) => {
                listener.ok()
            }
            _ => None,
            // TODO log
        };
        #[cfg(not(unix))]
        let listener = if &self.listen_address != &configuration.server.listen {
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
        #[cfg(unix)]
        match handle.listen_address() {
            AnyAddr::Left(tcp_addr) => {
                tracing::debug!("Restarted on {}", tcp_addr)
            }
            AnyAddr::Right(unix_addr) => {
                tracing::debug!("Restarted on {:?}", unix_addr)
            }
        }
        #[cfg(not(unix))]
        tracing::debug!("Restarted on {}", handle.listen_address());

        Ok(handle)
    }

    pub(crate) fn listen_address(&self) -> &AnyAddr {
        &self.listen_address
    }
}

/// A trait for a listener: `TcpListener` and `UnixListener`.
pub trait Listener: Send + Unpin {
    /// The stream's type of this listener.
    type Io: tokio::io::AsyncRead + tokio::io::AsyncWrite;
    /// The socket address type of this listener.
    type Addr;

    /// Accepts a new incoming connection from this listener.
    fn accept<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = io::Result<(Self::Io, Self::Addr)>> + Send + 'a>>;

    /// Returns the local address that this listener is bound to.
    fn local_addr(&self) -> io::Result<Self::Addr>;
}

impl Listener for tokio::net::TcpListener {
    type Io = tokio::net::TcpStream;
    type Addr = std::net::SocketAddr;

    fn accept<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = io::Result<(Self::Io, Self::Addr)>> + Send + 'a>> {
        let accept = self.accept();
        Box::pin(async {
            let (stream, addr) = accept.await?;
            Ok((stream, addr.into()))
        })
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.local_addr().map(Into::into)
    }
}

#[cfg(unix)]
impl Listener for tokio::net::UnixListener {
    type Io = tokio::net::UnixStream;
    type Addr = tokio::net::unix::SocketAddr;

    fn accept<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = io::Result<(Self::Io, Self::Addr)>> + Send + 'a>> {
        let accept = self.accept();
        Box::pin(async {
            let (stream, addr) = accept.await?;
            Ok((stream, addr.into()))
        })
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.local_addr().map(Into::into)
    }
}

impl<L, R> Listener for Either<L, R>
where
    L: Listener,
    R: Listener,
{
    type Io = Either<<L as Listener>::Io, <R as Listener>::Io>;
    type Addr = Either<<L as Listener>::Addr, <R as Listener>::Addr>;

    fn accept<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = io::Result<(Self::Io, Self::Addr)>> + Send + 'a>> {
        match self {
            Either::Left(listener) => {
                let fut = listener.accept();
                Box::pin(async move {
                    let (stream, addr) = fut.await?;
                    Ok((Either::Left(stream), Either::Left(addr)))
                })
            }
            Either::Right(listener) => {
                let fut = listener.accept();
                Box::pin(async move {
                    let (stream, addr) = fut.await?;
                    Ok((Either::Right(stream), Either::Right(addr)))
                })
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        match self {
            Either::Left(listener) => {
                let addr = listener.local_addr()?;
                Ok(Either::Left(addr))
            }
            Either::Right(listener) => {
                let addr = listener.local_addr()?;
                Ok(Either::Right(addr))
            }
        }
    }
}

#[cfg(unix)]
pub(crate) type AnyListener = Either<tokio::net::TcpListener, tokio::net::UnixListener>;
#[cfg(unix)]
pub(crate) type AnyAddr = Either<std::net::SocketAddr, tokio::net::unix::SocketAddr>;
#[cfg(not(unix))]
pub(crate) type AnyListener = tokio::net::TcpListener;
#[cfg(not(unix))]
pub(crate) type AnyAddr = std::net::SocketAddr;

/*
impl Listener for BoxedListener {
    type Io = BoxedListenerStream;
}
*/

/*
pub trait ErasedListener {
    /// Accepts a new incoming connection from this listener.
    fn accept<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = io::Result<(Self::Io, Self::Addr)>> + Send + 'a>>;

    /// Returns the local address that this listener is bound to.
    fn local_addr(&self) -> io::Result<Self::Addr>;
}
*/

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
        let temp_path = tempfile::NamedTempFile::new().unwrap().into_temp_path();
        std::fs::remove_file(&temp_path).unwrap();
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let listener = Box::new(UnixListener::bind(&temp_path).unwrap()) as Box<_>;

        HttpServerHandle::new(
            shutdown_sender,
            futures::future::ready(Ok(listener)).boxed(),
            ListenAddr::UnixSocket((&temp_path).into()),
        )
        .shutdown()
        .await
        .expect("Should have waited for shutdown");

        shutdown_receiver
            .await
            .expect("Should have been send notification to shutdown");
    }
}
