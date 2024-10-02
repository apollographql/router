use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::net::ToSocketAddrs;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use hickory_resolver::config::LookupIpStrategy;
use hickory_resolver::system_conf::read_system_conf;
use hickory_resolver::TokioAsyncResolver;
use hyper::client::connect::dns::Name;
use hyper::client::HttpConnector;
use hyper::service::Service;

use crate::configuration::shared::DnsResolutionStrategy;

/// Wrapper around hickory-resolver's
/// [`TokioAsyncResolver`](https://docs.rs/hickory-resolver/0.24.1/hickory_resolver/type.TokioAsyncResolver.html)
///
/// The resolver runs a background Task which manages dns requests. When a new resolver is created,
/// the background task is also created, it needs to be spawned on top of an executor before using the client,
/// or dns requests will block.
#[derive(Debug, Clone)]
pub(crate) struct AsyncHyperResolver(TokioAsyncResolver);

impl AsyncHyperResolver {
    /// constructs a new resolver from default configuration, using [read_system_conf](https://docs.rs/hickory-resolver/0.24.1/hickory_resolver/system_conf/fn.read_system_conf.html)
    fn new_from_system_conf(
        dns_resolution_strategy: DnsResolutionStrategy,
    ) -> Result<Self, io::Error> {
        let (config, mut options) = read_system_conf()?;
        options.ip_strategy = dns_resolution_strategy.into();

        Ok(Self(TokioAsyncResolver::tokio(config, options)))
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

impl From<DnsResolutionStrategy> for LookupIpStrategy {
    fn from(value: DnsResolutionStrategy) -> LookupIpStrategy {
        match value {
            DnsResolutionStrategy::Ipv4Only => LookupIpStrategy::Ipv4Only,
            DnsResolutionStrategy::Ipv6Only => LookupIpStrategy::Ipv6Only,
            DnsResolutionStrategy::Ipv4AndIpv6 => LookupIpStrategy::Ipv4AndIpv6,
            DnsResolutionStrategy::Ipv6ThenIpv4 => LookupIpStrategy::Ipv6thenIpv4,
            DnsResolutionStrategy::Ipv4ThenIpv6 => LookupIpStrategy::Ipv4thenIpv6,
        }
    }
}

/// A helper function to create an http connector and a dns task with the default configuration
pub(crate) fn new_async_http_connector(
    dns_resolution_strategy: DnsResolutionStrategy,
) -> Result<HttpConnector<AsyncHyperResolver>, io::Error> {
    let resolver = AsyncHyperResolver::new_from_system_conf(dns_resolution_strategy)?;
    Ok(HttpConnector::new_with_resolver(resolver))
}
