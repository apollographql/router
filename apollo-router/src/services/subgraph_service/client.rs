use std::{
    collections::HashSet,
    iter::{once, Once},
    net::SocketAddr,
    pin::Pin,
    str::FromStr,
    task::{Context, Poll},
};

use futures::{
    future::{self, Ready},
    Future, Stream,
};
use http::{Request, StatusCode};
use hyper::{
    client::{
        conn,
        connect::dns::{GaiFuture, GaiResolver, Name},
        HttpConnector,
    },
    Body,
};
use pin_project_lite::pin_project;
use tokio::net::TcpStream;
use tower::{
    discover::{Change, Discover},
    Service, ServiceExt,
};

struct Client {
    url: String,
}

#[derive(Clone)]
struct StaticResolver {
    addr: SocketAddr,
}

impl Service<Name> for StaticResolver {
    type Response = Once<SocketAddr>;
    type Error = std::io::Error;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: Name) -> Self::Future {
        println!("called static resolver for address {}", self.addr);
        future::ok(once(self.addr))
    }
}

pin_project! {
    struct DnsDiscovery {
        fqdn: Name,
        resolver: GaiResolver,
        addresses: HashSet<SocketAddr>,
        changes: Vec<Change<Key, hyper::Client<HttpConnector<StaticResolver>>>>,
        fut: Option<GaiFuture>,
    }
}

impl DnsDiscovery {
    pub(crate) fn new(fqdn: String) -> Self {
        Self {
            fqdn: Name::from_str(&fqdn).unwrap(),
            resolver: GaiResolver::new(),
            addresses: HashSet::new(),
            changes: vec![],
            fut: None,
        }
    }
}

type Error = Box<dyn std::error::Error + Send + Sync>;

type Key = SocketAddr;

impl Stream for DnsDiscovery {
    type Item = Result<Change<Key, hyper::Client<HttpConnector<StaticResolver>>>, Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        loop {
            if !this.changes.is_empty() {
                println!("changes are now: {:?}", this.changes);
                return Poll::Ready(this.changes.pop().map(Ok));
            }

            if this.fut.is_none() {
                match this.resolver.poll_ready(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Err(e)) => {
                        println!("error resolving: {e}");
                        return Poll::Ready(None);
                    }
                    Poll::Ready(Ok(())) => *this.fut = Some(this.resolver.call(this.fqdn.clone())),
                }
            }

            let addresses;
            match this.fut {
                //FIXME: not sure
                None => return Poll::Ready(None),
                Some(fut) => match Pin::new(fut).poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(res) => addresses = res,
                },
            }
            *this.fut = None;

            match addresses {
                Err(e) => {
                    println!("error resolving: {e}");
                    return Poll::Ready(None);
                }
                Ok(ad) => {
                    let set = ad.into_iter().collect::<HashSet<_>>();
                    println!("DNS lookup returned {set:?}");

                    for address in this.addresses.difference(&set) {
                        this.changes.push(Change::Remove(*address));
                    }

                    for address in set.difference(&this.addresses) {
                        let resolver = StaticResolver { addr: *address };
                        let mut http_connector = HttpConnector::new_with_resolver(resolver);
                        http_connector.set_nodelay(true);
                        http_connector.set_keepalive(Some(std::time::Duration::from_secs(60)));
                        http_connector.enforce_http(false);
                        this.changes.push(Change::Insert(
                            *address,
                            hyper::Client::builder().build(http_connector),
                        ));
                    }
                    *this.addresses = set;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use futures::lock::Mutex;
    use tower::{
        balance::p2c::{self, Balance},
        load,
    };

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn test() {
        let decay = Duration::from_secs(10);
        let mut p2c = Arc::new(Mutex::new(p2c::Balance::new(load::PeakEwmaDiscover::new(
            DnsDiscovery::new("local.apollo.dev".to_string()),
            Duration::from_secs(2),
            decay,
            load::CompleteOnResponse::default(),
        ))));
        /*let mut p2c = p2c::MakeBalance::new(load::PeakEwmaDiscover::new(
            DnsDiscovery::new("local.apollo.dev".to_string()),
            Duration::from_secs(2),
            decay,
            load::CompleteOnResponse::default(),
        ));*/

        let balance = p2c.clone();

        let handle = tokio::task::spawn(async move {
            println!("3.");
            balance
                .lock()
                .await
                .ready()
                .await
                .unwrap()
                .call(
                    Request::builder()
                        .uri("http://local.apollo.dev:4001/")
                        .method("GET")
                        .body(Body::from(""))
                        .unwrap(),
                )
                .await
                .unwrap();

            println!("4.");
            balance
                .lock()
                .await
                .ready()
                .await
                .unwrap()
                .call(
                    Request::builder()
                        .uri("http://local.apollo.dev:4001/")
                        .method("GET")
                        .body(Body::from(""))
                        .unwrap(),
                )
                .await
                .unwrap();
        });
        println!("1.");
        p2c.lock()
            .await
            .ready()
            .await
            .unwrap()
            .call(
                Request::builder()
                    .uri("http://local.apollo.dev:4001/")
                    .method("GET")
                    .body(Body::from(""))
                    .unwrap(),
            )
            .await
            .unwrap();

        println!("2.");
        p2c.lock()
            .await
            .ready()
            .await
            .unwrap()
            .call(
                Request::builder()
                    .uri("http://local.apollo.dev:4001/")
                    .method("GET")
                    .body(Body::from(""))
                    .unwrap(),
            )
            .await
            .unwrap();

        handle.await;
        panic!();
    }
}
