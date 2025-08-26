use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use bytes::Buf;
use http_body::SizeHint;
use pin_project_lite::pin_project;
use tokio::sync::OwnedSemaphorePermit;

use super::layer::BodyLimitControl;

pin_project! {
    /// An implementation of http_body::Body that limits the number of bytes read from the inner body.
    /// Unlike the `RequestBodyLimit` middleware, this will always return Pending if the inner body has exceeded the limit.
    /// Upon reaching the limit the guard will be dropped allowing the RequestBodyLimitLayer to return.
    pub(crate) struct Limited<Body> {
        #[pin]
        inner: Body,
        #[pin]
        permit: ForgetfulPermit,
        control: BodyLimitControl,
    }
}

impl<Body> Limited<Body>
where
    Body: http_body::Body,
{
    pub(super) fn new(
        inner: Body,
        control: BodyLimitControl,
        permit: OwnedSemaphorePermit,
    ) -> Self {
        Self {
            inner,
            control,
            permit: permit.into(),
        }
    }
}

struct ForgetfulPermit(Option<OwnedSemaphorePermit>);

impl ForgetfulPermit {
    fn release(&mut self) {
        self.0.take();
    }
}

impl Drop for ForgetfulPermit {
    fn drop(&mut self) {
        // If the limit was not hit we must not release the guard otherwise a response of 413 will be returned.
        // This may be because the inner body was not fully read.
        // Instead we must forget the permit.
        if let Some(permit) = self.0.take() {
            permit.forget();
        }
    }
}

impl From<OwnedSemaphorePermit> for ForgetfulPermit {
    fn from(permit: OwnedSemaphorePermit) -> Self {
        Self(Some(permit))
    }
}

impl<Body> http_body::Body for Limited<Body>
where
    Body: http_body::Body,
{
    type Data = Body::Data;
    type Error = Body::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let mut this = self.project();
        let res = match this.inner.poll_frame(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(None) => None,
            Poll::Ready(Some(Ok(frame))) => {
                let Some(data) = frame.data_ref() else {
                    return Poll::Ready(Some(Ok(frame)));
                };

                if data.remaining() > this.control.remaining() {
                    // This is the difference between http_body::Limited and our implementation.
                    // Dropping this mutex allows the containing layer to immediately return an error response
                    // This prevents the need to deal with wrapped errors.
                    this.permit.release();
                    return Poll::Pending;
                } else {
                    this.control.increment(data.remaining());
                    Some(Ok(frame))
                }
            }
            Poll::Ready(Some(Err(err))) => Some(Err(err)),
        };

        Poll::Ready(res)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        match u64::try_from(self.control.remaining()) {
            Ok(n) => {
                let mut hint = self.inner.size_hint();
                if hint.lower() >= n {
                    hint.set_exact(n)
                } else if let Some(max) = hint.upper() {
                    hint.set_upper(n.min(max))
                } else {
                    hint.set_upper(n)
                }
                hint
            }
            Err(_) => self.inner.size_hint(),
        }
    }
}

#[cfg(test)]
mod test {
    use std::pin::Pin;
    use std::sync::Arc;

    use bytes::Bytes;
    use http_body::Body;
    use tower::BoxError;

    use crate::plugins::limits::layer::BodyLimitControl;
    use crate::services::router::body;

    #[test]
    fn test_completes() {
        let control = BodyLimitControl::new(100);
        let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
        let lock = semaphore.clone().try_acquire_owned().unwrap();
        let mut limited = super::Limited::new(body::from_bytes("test".to_string()), control, lock);

        match Pin::new(&mut limited).poll_frame(&mut std::task::Context::from_waker(
            &futures::task::noop_waker(),
        )) {
            std::task::Poll::Ready(Some(Ok(data))) => {
                let data = data.into_data().unwrap().to_vec();
                let content = String::from_utf8_lossy(data.as_slice());
                assert_eq!(&content, "test");
            }
            std::task::Poll::Pending => panic!("it should be ready"),
            _ => panic!("the data returned is incorrect"),
        }

        assert!(semaphore.try_acquire().is_err());

        // We need to assert that if the stream is dropped the semaphore isn't released.
        // It's only explicitly hitting the limit that releases the semaphore.
        drop(limited);
        assert!(semaphore.try_acquire().is_err());
    }

    #[test]
    fn test_limit_hit() {
        let control = BodyLimitControl::new(1);
        let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
        let lock = semaphore.clone().try_acquire_owned().unwrap();
        let mut limited = super::Limited::new("test".to_string(), control, lock);

        match Pin::new(&mut limited).poll_frame(&mut std::task::Context::from_waker(
            &futures::task::noop_waker(),
        )) {
            std::task::Poll::Pending => {}
            std::task::Poll::Ready(_) => panic!("it should be pending"),
        }
        assert!(semaphore.try_acquire().is_ok())
    }

    #[test]
    fn test_limit_hit_after_multiple() {
        let control = BodyLimitControl::new(5);
        let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
        let lock = semaphore.clone().try_acquire_owned().unwrap();

        let mut limited = super::Limited::new(
            body::from_result_stream(futures::stream::iter(vec![
                Ok::<Bytes, BoxError>("hello".into()),
                Ok("world".into()),
            ])),
            control,
            lock,
        );
        match Pin::new(&mut limited).poll_frame(&mut std::task::Context::from_waker(
            &futures::task::noop_waker(),
        )) {
            std::task::Poll::Ready(Some(Ok(_))) => {}
            _ => panic!("it should be ready with Some(Ok(_)"),
        }
        assert!(semaphore.try_acquire().is_err());
        if Pin::new(&mut limited)
            .poll_frame(&mut std::task::Context::from_waker(
                &futures::task::noop_waker(),
            ))
            .is_ready()
        {
            panic!("it should be pending");
        }
        assert!(semaphore.try_acquire().is_ok());
    }
}
