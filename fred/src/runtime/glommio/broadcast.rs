use crate::error::Error;
use glommio::{
  channels::local_channel::{new_unbounded, LocalReceiver, LocalSender},
  GlommioError,
  ResourceType,
};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

struct Inner<T: Clone> {
  pub counter: u64,
  pub senders: BTreeMap<u64, LocalSender<T>>,
}

/// A multi-producer multi-consumer channel receiver.
///
/// See [LocalReceiver](glommio::channels::local_channel::LocalReceiver) for more information.
pub struct BroadcastReceiver<T: Clone> {
  id:    u64,
  inner: Rc<RefCell<Inner<T>>>,
  rx:    LocalReceiver<T>,
}

impl<T: Clone> BroadcastReceiver<T> {
  /// Receives data from this channel.
  ///
  /// See [recv](glommio::channels::local_channel::LocalReceiver::recv) for more information.
  pub async fn recv(&self) -> Result<T, Error> {
    match self.rx.recv().await {
      Some(v) => Ok(v),
      None => Err(Error::new_canceled()),
    }
  }
}

impl<T: Clone> Drop for BroadcastReceiver<T> {
  fn drop(&mut self) {
    self.inner.as_ref().borrow_mut().senders.remove(&self.id);
  }
}

#[derive(Clone)]
pub struct BroadcastSender<T: Clone> {
  inner: Rc<RefCell<Inner<T>>>,
}

impl<T: Clone> BroadcastSender<T> {
  pub fn new() -> Self {
    BroadcastSender {
      inner: Rc::new(RefCell::new(Inner {
        counter: 0,
        senders: BTreeMap::new(),
      })),
    }
  }

  pub fn subscribe(&self) -> BroadcastReceiver<T> {
    let (tx, rx) = new_unbounded();
    let id = {
      let mut guard = self.inner.as_ref().borrow_mut();
      let count = guard.counter.wrapping_add(1);
      guard.counter = count;
      guard.senders.insert(count, tx);
      guard.counter
    };

    BroadcastReceiver {
      id,
      rx,
      inner: self.inner.clone(),
    }
  }

  pub fn send<F: Fn(&T)>(&self, msg: &T, func: F) {
    let mut guard = self.inner.as_ref().borrow_mut();

    let to_remove: Vec<u64> = guard
      .senders
      .iter()
      .filter_map(|(id, tx)| {
        if let Err(GlommioError::Closed(ResourceType::Channel(val))) = tx.try_send(msg.clone()) {
          func(&val);
          Some(*id)
        } else {
          None
        }
      })
      .collect();

    for id in to_remove.into_iter() {
      guard.senders.remove(&id);
    }
  }
}
