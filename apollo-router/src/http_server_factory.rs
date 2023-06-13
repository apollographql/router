use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use derivative::Derivative;
use futures::channel::oneshot;
use futures::prelude::*;
use itertools::Itertools;
use multimap::MultiMap;
use tokio::sync::mpsc;

use super::router::ApolloRouterError;
use crate::configuration::Configuration;
use crate::configuration::ListenAddr;
use crate::router_factory::Endpoint;
use crate::router_factory::RouterFactory;
use crate::uplink::license_enforcement::LicenseState;

/// Factory for creating the http server component.
///
/// This trait enables us to test that `StateMachine` correctly recreates the http server when
/// necessary e.g. when listen address changes.
pub(crate) trait HttpServerFactory {
    type Future: Future<Output = Result<HttpServerHandle, ApolloRouterError>> + Send;

    #[allow(clippy::too_many_arguments)]
    fn create<RF>(
        &self,
        service_factory: RF,
        configuration: Arc<Configuration>,
        main_listener: Option<Listener>,
        previous_listeners: Vec<(ListenAddr, Listener)>,
        extra_endpoints: MultiMap<ListenAddr, Endpoint>,
        license: LicenseState,
        all_connections_stopped_sender: mpsc::Sender<()>,
    ) -> Self::Future
    where
        RF: RouterFactory;
}

type MainAndExtraListeners = (Listener, Vec<(ListenAddr, Listener)>);
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
        Pin<Box<dyn Future<Output = Result<MainAndExtraListeners, ApolloRouterError>> + Send>>,

    /// The listen addresses that the server is actually listening on.
    /// This includes the `graphql_listen_address` as well as any other address a plugin listens on.
    /// If a socket address specified port zero the OS will assign a random free port.
    listen_addresses: Vec<ListenAddr>,

    /// The listen addresses that the graphql server is actually listening on.
    /// If a socket address specified port zero the OS will assign a random free port.
    graphql_listen_address: Option<ListenAddr>,

    /// copied into every client session, to track if there are still running sessions when shutting down
    all_connections_stopped_sender: mpsc::Sender<()>,
}

impl HttpServerHandle {
    pub(crate) fn new(
        shutdown_sender: oneshot::Sender<()>,
        server_future: Pin<
            Box<
                dyn Future<Output = Result<MainAndExtraListeners, ApolloRouterError>>
                    + Send
                    + 'static,
            >,
        >,
        graphql_listen_address: Option<ListenAddr>,
        listen_addresses: Vec<ListenAddr>,
        all_connections_stopped_sender: mpsc::Sender<()>,
    ) -> Self {
        Self {
            shutdown_sender,
            server_future,
            graphql_listen_address,
            listen_addresses,
            all_connections_stopped_sender,
        }
    }

    pub(crate) async fn shutdown(self) -> Result<(), ApolloRouterError> {
        if let Err(_err) = self.shutdown_sender.send(()) {
            tracing::error!("Failed to notify http thread of shutdown")
        };
        let _listener = self.server_future.await?;
        #[cfg(unix)]
        // listen_addresses includes the main graphql_address
        for listen_address in self.listen_addresses {
            if let ListenAddr::UnixSocket(path) = listen_address {
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
        web_endpoints: MultiMap<ListenAddr, Endpoint>,
        license: LicenseState,
    ) -> Result<Self, ApolloRouterError>
    where
        SF: HttpServerFactory,
        RF: RouterFactory,
    {
        // we tell the currently running server to stop
        if let Err(_err) = self.shutdown_sender.send(()) {
            tracing::error!("Failed to notify http thread of shutdown")
        };

        // when the server receives the shutdown signal, it stops accepting new
        // connections, and returns the TCP listener, to reuse it in the next server
        // it is necessary to keep the queue of new TCP sockets associated with
        // the listener instead of dropping them
        let (main_listener, extra_listeners) = self.server_future.await?;
        tracing::debug!("previous server stopped");

        // we give the listeners to the new configuration, they'll clean up whatever needs to
        let handle = factory
            .create(
                router,
                configuration,
                Some(main_listener),
                extra_listeners,
                web_endpoints,
                license,
                self.all_connections_stopped_sender.clone(),
            )
            .await?;
        tracing::debug!(
            "restarted on {}",
            handle
                .listen_addresses()
                .iter()
                .map(std::string::ToString::to_string)
                .join(" - ")
        );

        Ok(handle)
    }

    pub(crate) fn listen_addresses(&self) -> &[ListenAddr] {
        self.listen_addresses.as_slice()
    }

    pub(crate) fn graphql_listen_address(&self) -> &Option<ListenAddr> {
        &self.graphql_listen_address
    }
}

pub(crate) enum Listener {
    Tcp(tokio::net::TcpListener),
    #[cfg(unix)]
    Unix(tokio::net::UnixListener),
    Tls {
        listener: tokio::net::TcpListener,
        acceptor: tokio_rustls::TlsAcceptor,
    },
}

pub(crate) enum NetworkStream {
    Tcp(tokio::net::TcpStream),
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
    Tls(tokio_rustls::server::TlsStream<tokio::net::TcpStream>),
}

impl Listener {
    pub(crate) async fn new_from_socket_addr(
        address: SocketAddr,
        tls_acceptor: Option<tokio_rustls::TlsAcceptor>,
    ) -> Result<Self, ApolloRouterError> {
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .map_err(ApolloRouterError::ServerCreationError)?;
        match tls_acceptor {
            None => Ok(Listener::Tcp(listener)),
            Some(acceptor) => Ok(Listener::Tls { listener, acceptor }),
        }
    }

    pub(crate) fn new_from_listener(
        listener: tokio::net::TcpListener,
        tls_acceptor: Option<tokio_rustls::TlsAcceptor>,
    ) -> Self {
        match tls_acceptor {
            None => Listener::Tcp(listener),
            Some(acceptor) => Listener::Tls { listener, acceptor },
        }
    }

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
            Listener::Tls { listener, .. } => listener.local_addr().map(Into::into),
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
            Listener::Tls { listener, acceptor } => {
                let (stream, _) = listener.accept().await?;

                Ok(NetworkStream::Tls(acceptor.accept(stream).await?))
            }
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
    // TODO [igni]: add a check with extra endpoints
    async fn sanity() {
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let listener = Listener::Tcp(tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap());
        let (all_connections_stopped_sender, _) = mpsc::channel::<()>(1);

        HttpServerHandle::new(
            shutdown_sender,
            futures::future::ready(Ok((listener, vec![]))).boxed(),
            Some(SocketAddr::from_str("127.0.0.1:0").unwrap().into()),
            Default::default(),
            all_connections_stopped_sender,
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
    // TODO [igni]: add a check with extra endpoints
    async fn sanity_unix() {
        let temp_dir = tempfile::tempdir().unwrap();
        let sock = temp_dir.as_ref().join("sock");
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let listener = Listener::Unix(tokio::net::UnixListener::bind(&sock).unwrap());
        let (all_connections_stopped_sender, _) = mpsc::channel::<()>(1);

        HttpServerHandle::new(
            shutdown_sender,
            futures::future::ready(Ok((listener, vec![]))).boxed(),
            Some(ListenAddr::UnixSocket(sock)),
            Default::default(),
            all_connections_stopped_sender,
        )
        .shutdown()
        .await
        .expect("Should have waited for shutdown");

        shutdown_receiver
            .await
            .expect("Should have sent notification to shutdown");
    }
}
