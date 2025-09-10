use futures::Stream;
use futures_lite::{future::poll_fn, FutureExt};
use glommio::{
  channels::local_channel::{new_bounded, new_unbounded, LocalReceiver, LocalSender},
  GlommioError,
};
use std::{
  ops::Deref,
  pin::Pin,
  rc::Rc,
  task::{Context, Poll},
};

pub fn channel<T: 'static>(size: usize) -> (Sender<T>, Receiver<T>) {
  if size == 0 {
    let (tx, rx) = new_unbounded();
    (tx.into(), rx.into())
  } else {
    let (tx, rx) = new_bounded(size);
    (tx.into(), rx.into())
  }
}

pub struct UnboundedReceiverStream<T> {
  rx: LocalReceiver<T>,
}

impl<T> From<LocalReceiver<T>> for UnboundedReceiverStream<T> {
  fn from(rx: LocalReceiver<T>) -> Self {
    UnboundedReceiverStream { rx }
  }
}

impl<T> UnboundedReceiverStream<T> {
  #[allow(dead_code)]
  pub async fn recv(&mut self) -> Option<T> {
    self.rx.recv().await
  }
}

impl<T> Stream for UnboundedReceiverStream<T> {
  type Item = T;

  fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
    use futures_lite::stream::StreamExt;
    self.rx.stream().poll_next(cx)
  }
}

pub struct Receiver<T: 'static> {
  rx: LocalReceiver<T>,
}

impl<T: 'static> From<LocalReceiver<T>> for Receiver<T> {
  fn from(rx: LocalReceiver<T>) -> Self {
    Receiver { rx }
  }
}

impl<T: 'static> Receiver<T> {
  pub async fn recv(&mut self) -> Option<T> {
    self.rx.recv().await
  }

  pub fn into_stream(self) -> impl Stream<Item = T> + 'static {
    // what happens if we `join` the futures from `recv()` and `rx.stream().next()`?
    UnboundedReceiverStream::from(self.rx)
  }

  // despite being async this works similar to Tokio's try_recv in that it won't actually await on anything. the async
  // wrapper is used so it works with `poll_fn`
  pub async fn try_recv(&mut self) -> Option<T> {
    let mut ft = Box::pin(self.rx.recv());

    poll_fn(|cx| match ft.poll(cx) {
      Poll::Pending | Poll::Ready(None) => Poll::Ready(None),
      Poll::Ready(Some(v)) => Poll::Ready(Some(v)),
    })
    .await
  }
}

pub struct Sender<T: 'static> {
  tx: Rc<LocalSender<T>>,
}

// https://github.com/rust-lang/rust/issues/26925
impl<T: 'static> Clone for Sender<T> {
  fn clone(&self) -> Self {
    Sender { tx: self.tx.clone() }
  }
}

impl<T: 'static> From<LocalSender<T>> for Sender<T> {
  fn from(tx: LocalSender<T>) -> Self {
    Sender { tx: Rc::new(tx) }
  }
}

impl<T: 'static> Sender<T> {
  pub fn try_send(&self, msg: T) -> Result<(), GlommioError<T>> {
    self.tx.try_send(msg)
  }

  pub async fn send(&self, msg: T) -> Result<(), GlommioError<T>> {
    self.tx.deref().send(msg).await
  }
}
