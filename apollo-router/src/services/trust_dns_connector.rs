use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::net::ToSocketAddrs;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use hyper::client::connect::dns::Name;
use hyper::client::HttpConnector;
use hyper::service::Service;
use trust_dns_resolver::TokioAsyncResolver;

/// Wrapper around trust-dns-resolver's
/// [`TokioAsyncResolver`](https://docs.rs/trust-dns-resolver/0.23.2/trust_dns_resolver/type.TokioAsyncResolver.html)
///
/// The resolver runs a background Task which manages dns requests. When a new resolver is created,
/// the background task is also created, it needs to be spawned on top of an executor before using the client,
/// or dns requests will block.
#[derive(Debug, Clone)]
pub(crate) struct AsyncHyperResolver(TokioAsyncResolver);

impl AsyncHyperResolver {
    /// constructs a new resolver from default configuration, uses the corresponding method of
    /// [`TokioAsyncResolver`](https://docs.rs/trust-dns-resolver/0.23.2/trust_dns_resolver/type.TokioAsyncResolver.html#method.new)
    pub(crate) fn new_from_system_conf() -> Result<Self, io::Error> {
        let resolver = TokioAsyncResolver::tokio_from_system_conf()?;
        Ok(Self(resolver))
    }
}

impl Service<Name> for AsyncHyperResolver {
    type Response = std::vec::IntoIter<SocketAddr>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
    type Error = io::Error;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, name: Name) -> Self::Future {
        let resolver = self.0.clone();

        Box::pin(async move {
            Ok(resolver
                .lookup_ip(name.as_str())
                .await?
                .iter()
                .map(|addr| (addr, 0_u16).to_socket_addrs())
                .try_fold(Vec::new(), |mut acc, s_addr| {
                    acc.extend(s_addr?);
                    Ok::<_, io::Error>(acc)
                })?
                .into_iter())
        })
    }
}

/// A helper function to create an http connector and a dns task with the default configuration
pub(crate) fn new_async_http_connector() -> Result<HttpConnector<AsyncHyperResolver>, io::Error> {
    let resolver = AsyncHyperResolver::new_from_system_conf()?;
    Ok(HttpConnector::new_with_resolver(resolver))
}
