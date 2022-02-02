use futures::Future;
use futures_lite::future::FutureExt;
use moka::future::Cache;
use pin_project::pin_project;
use std::hash::Hash;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower::util::BoxCloneService;
use tower::Service;

pub enum State<ServiceFuture, CacheInsertFuture> {
    CacheGet,
    ServiceResolution(ServiceFuture),
    CacheInsert(CacheInsertFuture),
}

#[pin_project]
pub struct CacheResponseFuture<InnerService, Request>
where
    InnerService: Service<Request>,
    Request: Clone + Hash + Eq + Send + Sync,
    <InnerService as Service<Request>>::Response: Send + Sync + Clone + 'static,
    <InnerService as Service<Request>>::Error: Send + Sync + Clone + 'static,
    <InnerService as Service<Request>>::Future: Send,
{
    service: BoxCloneService<Request, InnerService::Response, InnerService::Error>,
    request: Request,
    cache: Cache<Request, InnerService::Response>,
    state: State<
        Pin<Box<dyn Future<Output = Result<InnerService::Response, InnerService::Error>>>>,
        Pin<Box<dyn Future<Output = ()>>>,
    >,
    response: Option<InnerService::Response>,
}

impl<InnerService, Request> CacheResponseFuture<InnerService, Request>
where
    InnerService: Service<Request>,
    Request: Clone + Hash + Eq + Send + Sync,
    <InnerService as Service<Request>>::Response: Send + Sync + Clone + 'static,
    <InnerService as Service<Request>>::Error: Send + Sync + Clone + 'static,
    <InnerService as Service<Request>>::Future: Send,
{
    pub(crate) fn new(
        service: BoxCloneService<Request, InnerService::Response, InnerService::Error>,
        request: Request,
        cache: Cache<Request, InnerService::Response>,
    ) -> Self {
        CacheResponseFuture {
            service,
            request,
            cache,
            state: State::CacheGet,
            response: None,
        }
    }
}

impl<S, Request> Future for CacheResponseFuture<S, Request>
where
    S: Service<Request>,
    Request: Clone + Hash + Eq + Send + Sync + 'static,
    <S as Service<Request>>::Response: Send + Sync + Clone + 'static,
    <S as Service<Request>>::Error: Send + Sync + Clone + 'static,
    <S as Service<Request>>::Future: Send,
{
    type Output = Result<S::Response, S::Error>;

    fn poll<'cache>(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let (new_state, maybe_response) = match this.state {
            State::CacheInsert(future) => match future.poll(cx) {
                Poll::Pending => {
                    return Poll::Pending;
                }
                // return regardless of whether the insert went wrong or not
                Poll::Ready(_) => {
                    return Poll::Ready(Ok(this
                        .response
                        .clone()
                        .expect("response has been inserted in the previous poll; qed")));
                }
            },
            State::ServiceResolution(future) => {
                match future.boxed_local().poll(cx) {
                    Poll::Pending => {
                        return Poll::Pending;
                    }
                    Poll::Ready(Ok(response)) => {
                        let request = this.request.clone();
                        // clone is an arc, we re fine
                        let cache_clone = this.cache.clone();
                        let response_clone = response.clone();
                        let insert_fut =
                            async move { cache_clone.insert(request, response_clone).await };
                        let local_box = insert_fut.boxed_local();
                        (State::CacheInsert(local_box), Some(response))
                    }
                    Poll::Ready(Err(error)) => {
                        return Poll::Ready(Err(error));
                    }
                }
            }
            State::CacheGet => {
                let maybe_response = this.cache.get(this.request);
                if let Some(response) = maybe_response {
                    return Poll::Ready(Ok(response));
                }
                let resolution_fut = this.service.call(this.request.clone()).boxed_local();
                (State::ServiceResolution(resolution_fut), None)
            }
        };

        *this.state = new_state;
        *this.response = maybe_response;

        return Poll::Pending;
    }
}

pub struct CachingService<InnerService, Request>
where
    InnerService: Service<Request>,
    Request: Clone + Hash + Eq + Send + Sync,
    InnerService::Response: Send + Sync + Clone,
{
    service: BoxCloneService<Request, InnerService::Response, InnerService::Error>,
    cache: Cache<Request, InnerService::Response>,
}

impl<InnerService, Request> CachingService<InnerService, Request>
where
    InnerService: Service<Request> + Clone + Send + 'static,
    Request: Clone + Hash + Eq + Send + Sync,
    InnerService::Response: Send + Sync + Clone + 'static,
    InnerService::Future: Send + 'static,
    Request: 'static,
{
    pub fn from_service(
        service: BoxCloneService<Request, InnerService::Response, InnerService::Error>,
    ) -> Self {
        Self {
            service,
            cache: Cache::new(4096),
        }
    }
}

impl<InnerService, Request> Service<Request> for CachingService<InnerService, Request>
where
    Request: Clone + Hash + Eq + Send + Sync + 'static,
    InnerService::Response: Send + Sync + Clone + 'static,
    InnerService::Error: Send + Sync + Clone + 'static,
    InnerService: Service<Request>,
    InnerService::Future: Send,
{
    type Response = InnerService::Response;
    type Error = InnerService::Error;
    type Future = CacheResponseFuture<InnerService, Request>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        CacheResponseFuture::new(self.service.clone(), request, self.cache.clone())
    }
}
