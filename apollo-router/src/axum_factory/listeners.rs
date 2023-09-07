//! Listeners and endpoints

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use axum::response::*;
use axum::Router;
use futures::channel::oneshot;
use futures::prelude::*;
use hyper::server::conn::Http;
use multimap::MultiMap;
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio::sync::Notify;

use crate::configuration::Configuration;
use crate::http_server_factory::Listener;
use crate::http_server_factory::NetworkStream;
use crate::router::ApolloRouterError;
use crate::router_factory::Endpoint;
use crate::ListenAddr;

#[derive(Clone, Debug)]
pub(crate) struct ListenAddrAndRouter(pub(crate) ListenAddr, pub(crate) Router);

#[derive(Debug)]
pub(crate) struct ListenersAndRouters {
    pub(crate) main: ListenAddrAndRouter,
    pub(crate) extra: MultiMap<ListenAddr, Router>,
}

/// Merging [`axum::Router`]`s that use the same path panics (yes it doesn't raise an error, it panics.)
///
/// In order to not crash the router if paths clash using hot reload, we make sure the configuration is consistent,
/// and raise an error instead.
pub(super) fn ensure_endpoints_consistency(
    configuration: &Configuration,
    endpoints: &MultiMap<ListenAddr, Endpoint>,
) -> Result<(), ApolloRouterError> {
    // check the main endpoint
    if let Some(supergraph_listen_endpoint) = endpoints.get_vec(&configuration.supergraph.listen) {
        if supergraph_listen_endpoint
            .iter()
            .any(|e| e.path == configuration.supergraph.path)
        {
            if let Some((ip, port)) = configuration.supergraph.listen.ip_and_port() {
                return Err(ApolloRouterError::SameRouteUsedTwice(
                    ip,
                    port,
                    configuration.supergraph.path.clone(),
                ));
            }
        }
    }

    // check the extra endpoints
    let mut listen_addrs_and_paths = HashSet::new();
    for (listen, endpoints) in endpoints.iter_all() {
        for endpoint in endpoints {
            if let Some((ip, port)) = listen.ip_and_port() {
                if !listen_addrs_and_paths.insert((ip, port, endpoint.path.clone())) {
                    return Err(ApolloRouterError::SameRouteUsedTwice(
                        ip,
                        port,
                        endpoint.path.clone(),
                    ));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn extra_endpoints(
    endpoints: MultiMap<ListenAddr, Endpoint>,
) -> MultiMap<ListenAddr, Router> {
    let mut mm: MultiMap<ListenAddr, axum::Router> = Default::default();
    mm.extend(endpoints.into_iter().map(|(listen_addr, e)| {
        (
            listen_addr,
            e.into_iter().map(|e| e.into_router()).collect::<Vec<_>>(),
        )
    }));
    mm
}

/// Binding different listen addresses to the same port will "relax" the requirements, which
/// could result in a security issue:
/// If endpoint A is exposed to 127.0.0.1:4000/foo and endpoint B is exposed to 0.0.0.0:4000/bar
/// 0.0.0.0:4000/foo would be accessible.
///
/// `ensure_listenaddrs_consistency` makes sure listen addresses that bind to the same port
/// have the same IP:
/// 127.0.0.1:4000 and 127.0.0.1:4000 will not trigger an error
/// 127.0.0.1:4000 and 0.0.0.0:4001 will not trigger an error
///
/// 127.0.0.1:4000 and 0.0.0.0:4000 will trigger an error
pub(super) fn ensure_listenaddrs_consistency(
    configuration: &Configuration,
    endpoints: &MultiMap<ListenAddr, Endpoint>,
) -> Result<(), ApolloRouterError> {
    let mut all_ports = HashMap::new();
    if let Some((main_ip, main_port)) = configuration.supergraph.listen.ip_and_port() {
        all_ports.insert(main_port, main_ip);
    }

    if configuration.health_check.enabled {
        if let Some((ip, port)) = configuration.health_check.listen.ip_and_port() {
            if let Some(previous_ip) = all_ports.insert(port, ip) {
                if ip != previous_ip {
                    return Err(ApolloRouterError::DifferentListenAddrsOnSamePort(
                        previous_ip,
                        ip,
                        port,
                    ));
                }
            }
        }
    }

    for addr in endpoints.keys() {
        if let Some((ip, port)) = addr.ip_and_port() {
            if let Some(previous_ip) = all_ports.insert(port, ip) {
                if ip != previous_ip {
                    return Err(ApolloRouterError::DifferentListenAddrsOnSamePort(
                        previous_ip,
                        ip,
                        port,
                    ));
                }
            }
        }
    }

    Ok(())
}

pub(super) async fn get_extra_listeners(
    previous_listeners: Vec<(ListenAddr, Listener)>,
    mut extra_routers: MultiMap<ListenAddr, Router>,
) -> Result<Vec<((ListenAddr, Listener), axum::Router)>, ApolloRouterError> {
    let mut listeners_and_routers: Vec<((ListenAddr, Listener), axum::Router)> =
        Vec::with_capacity(extra_routers.len());

    // reuse previous extra listen addrs
    for (listen_addr, listener) in previous_listeners.into_iter() {
        if let Some(routers) = extra_routers.remove(&listen_addr) {
            listeners_and_routers.push((
                (listen_addr, listener),
                routers
                    .iter()
                    .fold(axum::Router::new(), |acc, r| acc.merge(r.clone())),
            ));
        }
    }

    // populate the new listen addrs
    for (listen_addr, routers) in extra_routers.into_iter() {
        // if we received a TCP listener, reuse it, otherwise create a new one
        #[cfg_attr(not(unix), allow(unused_mut))]
        let listener = match listen_addr.clone() {
            ListenAddr::SocketAddr(addr) => Listener::new_from_socket_addr(addr, None).await?,
            #[cfg(unix)]
            ListenAddr::UnixSocket(path) => Listener::Unix(
                UnixListener::bind(path).map_err(ApolloRouterError::ServerCreationError)?,
            ),
        };
        listeners_and_routers.push((
            (listen_addr, listener),
            routers
                .iter()
                .fold(axum::Router::new(), |acc, r| acc.merge(r.clone())),
        ));
    }

    Ok(listeners_and_routers)
}

pub(super) fn serve_router_on_listen_addr(
    mut listener: Listener,
    address: ListenAddr,
    router: axum::Router,
    all_connections_stopped_sender: mpsc::Sender<()>,
) -> (impl Future<Output = Listener>, oneshot::Sender<()>) {
    let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
    // this server reproduces most of hyper::server::Server's behaviour
    // we select over the stop_listen_receiver channel and the listener's
    // accept future. If the channel received something or the sender
    // was dropped, we stop using the listener and send it back through
    // listener_receiver
    let server = async move {
        tokio::pin!(shutdown_receiver);

        let connection_shutdown = Arc::new(Notify::new());
        let mut max_open_file_warning = None;

        let address = address.to_string();

        loop {
            tokio::select! {
                _ = &mut shutdown_receiver => {
                    break;
                }
                res = listener.accept() => {
                    let app = router.clone();
                    let connection_shutdown = connection_shutdown.clone();
                    let connection_stop_signal = all_connections_stopped_sender.clone();

                    match res {
                        Ok(res) => {
                            if max_open_file_warning.is_some(){
                                tracing::info!("can accept connections again");
                                max_open_file_warning = None;
                            }

                            tracing::info!(
                                counter.apollo_router_session_count_total = 1i64,
                                listener = &address
                            );

                            let address = address.clone();
                            tokio::task::spawn(async move {
                                // this sender must be moved into the session to track that it is still running
                                let _connection_stop_signal = connection_stop_signal;

                                match res {
                                    NetworkStream::Tcp(stream) => {
                                        stream
                                            .set_nodelay(true)
                                            .expect(
                                                "this should not fail unless the socket is invalid",
                                            );
                                            let connection = Http::new()
                                            .http1_keep_alive(true)
                                            .http1_header_read_timeout(Duration::from_secs(10))
                                            .serve_connection(stream, app);

                                        tokio::pin!(connection);
                                        tokio::select! {
                                            // the connection finished first
                                            _res = &mut connection => {
                                            }
                                            // the shutdown receiver was triggered first,
                                            // so we tell the connection to do a graceful shutdown
                                            // on the next request, then we wait for it to finish
                                            _ = connection_shutdown.notified() => {
                                                let c = connection.as_mut();
                                                c.graceful_shutdown();

                                                let _= connection.await;
                                            }
                                        }
                                    }
                                    #[cfg(unix)]
                                    NetworkStream::Unix(stream) => {
                                        let connection = Http::new()
                                        .http1_keep_alive(true)
                                        .serve_connection(stream, app);

                                        tokio::pin!(connection);
                                        tokio::select! {
                                            // the connection finished first
                                            _res = &mut connection => {
                                            }
                                            // the shutdown receiver was triggered first,
                                            // so we tell the connection to do a graceful shutdown
                                            // on the next request, then we wait for it to finish
                                            _ = connection_shutdown.notified() => {
                                                let c = connection.as_mut();
                                                c.graceful_shutdown();

                                                let _= connection.await;
                                            }
                                        }
                                    },
                                    NetworkStream::Tls(stream) => {
                                        stream.get_ref().0
                                            .set_nodelay(true)
                                            .expect(
                                                "this should not fail unless the socket is invalid",
                                            );

                                            let protocol = stream.get_ref().1.alpn_protocol();
                                            let http2 = protocol == Some(&b"h2"[..]);

                                            let connection = Http::new()
                                            .http1_keep_alive(true)
                                            .http1_header_read_timeout(Duration::from_secs(10))
                                            .http2_only(http2)
                                            .serve_connection(stream, app);

                                        tokio::pin!(connection);
                                        tokio::select! {
                                            // the connection finished first
                                            _res = &mut connection => {
                                            }
                                            // the shutdown receiver was triggered first,
                                            // so we tell the connection to do a graceful shutdown
                                            // on the next request, then we wait for it to finish
                                            _ = connection_shutdown.notified() => {
                                                let c = connection.as_mut();
                                                c.graceful_shutdown();

                                                let _= connection.await;
                                            }
                                        }
                                    }
                                }

                                tracing::info!(
                                    counter.apollo_router_session_count_total = -1i64,
                                    listener = &address
                                );

                            });
                        }

                        Err(e) => match e.kind() {
                            // this is already handled by moi and tokio
                            //std::io::ErrorKind::WouldBlock => todo!(),

                            // should be treated as EAGAIN
                            // https://man7.org/linux/man-pages/man2/accept.2.html
                            // Linux accept() (and accept4()) passes already-pending network
                            // errors on the new socket as an error code from accept().  This
                            // behavior differs from other BSD socket implementations.  For
                            // reliable operation the application should detect the network
                            // errors defined for the protocol after accept() and treat them
                            // like EAGAIN by retrying.  In the case of TCP/IP, these are
                            // ENETDOWN, EPROTO, ENOPROTOOPT, EHOSTDOWN, ENONET, EHOSTUNREACH,
                            // EOPNOTSUPP, and ENETUNREACH.
                            //
                            // those errors are not supported though: needs the unstable io_error_more feature
                            // std::io::ErrorKind::NetworkDown => todo!(),
                            // std::io::ErrorKind::HostUnreachable => todo!(),
                            // std::io::ErrorKind::NetworkUnreachable => todo!(),

                            //ECONNABORTED
                            std::io::ErrorKind::ConnectionAborted|
                            //EINTR
                            std::io::ErrorKind::Interrupted|
                            // EINVAL
                            std::io::ErrorKind::InvalidInput|
                            std::io::ErrorKind::PermissionDenied |
                            std::io::ErrorKind::TimedOut |
                            std::io::ErrorKind::ConnectionReset|
                            std::io::ErrorKind::NotConnected => {
                                // the socket was invalid (maybe timedout waiting in accept queue, or was closed)
                                // we should ignore that and get to the next one
                                continue;
                            }

                            // ignored errors, these should not happen with accept()
                            std::io::ErrorKind::NotFound |
                            std::io::ErrorKind::AddrInUse |
                            std::io::ErrorKind::AddrNotAvailable |
                            std::io::ErrorKind::BrokenPipe|
                            std::io::ErrorKind::AlreadyExists |
                            std::io::ErrorKind::InvalidData |
                            std::io::ErrorKind::WriteZero |
                            std::io::ErrorKind::Unsupported |
                            std::io::ErrorKind::UnexpectedEof |
                            std::io::ErrorKind::OutOfMemory => {
                                continue;
                            }

                            // EPROTO, EOPNOTSUPP, EBADF, EFAULT, EMFILE, ENOBUFS, ENOMEM, ENOTSOCK
                            // We match on _ because max open file errors fall under ErrorKind::Uncategorized
                            _ => {
                                match e.raw_os_error() {
                                    Some(libc::EMFILE) | Some(libc::ENFILE) => {
                                        match max_open_file_warning {
                                            None => {
                                                tracing::error!("reached the max open file limit, cannot accept any new connection");
                                                max_open_file_warning = Some(Instant::now());
                                            }
                                            Some(last) => if Instant::now() - last > Duration::from_secs(60) {
                                                tracing::error!("still at the max open file limit, cannot accept any new connection");
                                                max_open_file_warning = Some(Instant::now());
                                            }
                                        }
                                        tokio::time::sleep(Duration::from_millis(1)).await;
                                    }
                                    _ => {}
                                }
                                continue;
                            }

                        }
                    }
                }
            }
        }

        // the shutdown receiver was triggered so we break out of
        // the server loop, tell the currently active connections to stop
        // then return the TCP listen socket
        connection_shutdown.notify_waiters();
        listener
    };
    (server, shutdown_sender)
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::str::FromStr;

    use axum::BoxError;
    use tower::service_fn;
    use tower::ServiceExt;

    use super::*;
    use crate::axum_factory::tests::init_with_config;
    use crate::configuration::Sandbox;
    use crate::configuration::Supergraph;
    use crate::services::router;
    use crate::services::router_service;

    #[tokio::test]
    async fn it_makes_sure_same_listenaddrs_are_accepted() {
        let configuration = Configuration::fake_builder().build().unwrap();

        init_with_config(
            router_service::empty().await,
            Arc::new(configuration),
            MultiMap::new(),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn it_makes_sure_different_listenaddrs_but_same_port_are_not_accepted() {
        let configuration = Configuration::fake_builder()
            .supergraph(
                Supergraph::fake_builder()
                    .listen(SocketAddr::from_str("127.0.0.1:4010").unwrap())
                    .build(),
            )
            .sandbox(Sandbox::fake_builder().build())
            .build()
            .unwrap();

        let endpoint = service_fn(|req: router::Request| async move {
            Ok::<_, BoxError>(router::Response {
                response: http::Response::builder()
                    .body::<hyper::Body>("this is a test".to_string().into())
                    .unwrap(),
                context: req.context,
            })
        })
        .boxed();

        let mut web_endpoints = MultiMap::new();
        web_endpoints.insert(
            SocketAddr::from_str("0.0.0.0:4010").unwrap().into(),
            Endpoint::from_router_service("/".to_string(), endpoint),
        );

        let error = init_with_config(
            router_service::empty().await,
            Arc::new(configuration),
            web_endpoints,
        )
        .await
        .unwrap_err();
        assert_eq!(
            "tried to bind 127.0.0.1 and 0.0.0.0 on port 4010",
            error.to_string()
        )
    }

    #[tokio::test]
    async fn it_makes_sure_extra_endpoints_cant_use_the_same_listenaddr_and_path() {
        let configuration = Configuration::fake_builder()
            .supergraph(
                Supergraph::fake_builder()
                    .listen(SocketAddr::from_str("127.0.0.1:4010").unwrap())
                    .build(),
            )
            .build()
            .unwrap();
        let endpoint = service_fn(|req: router::Request| async move {
            Ok::<_, BoxError>(router::Response {
                response: http::Response::builder()
                    .body::<hyper::Body>("this is a test".to_string().into())
                    .unwrap(),
                context: req.context,
            })
        })
        .boxed();

        let mut mm = MultiMap::new();
        mm.insert(
            SocketAddr::from_str("127.0.0.1:4010").unwrap().into(),
            Endpoint::from_router_service("/".to_string(), endpoint),
        );

        let error = init_with_config(router_service::empty().await, Arc::new(configuration), mm)
            .await
            .unwrap_err();

        assert_eq!(
            "tried to register two endpoints on `127.0.0.1:4010/`",
            error.to_string()
        )
    }
}
