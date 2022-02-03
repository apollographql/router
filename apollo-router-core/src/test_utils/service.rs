use futures::{Future, FutureExt};
use mockall::automock;
use std::{pin::Pin, task::Poll};
use tower::{BoxError, Service};

#[automock]
pub trait TestService<Req, Res>
where
    Req: 'static,
    Res: Send + 'static,
{
    fn mock_call(&self, req: Req) -> Result<Res, BoxError>;
}

impl<Req, Res> Service<Req> for MockTestService<Req, Res>
where
    Res: Send + 'static,
{
    type Response = Res;

    type Error = BoxError;

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let res = self.mock_call(req);
        async move { res }.boxed()
    }
}
