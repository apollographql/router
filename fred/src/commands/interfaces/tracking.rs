use crate::{
  commands,
  interfaces::ClientLike,
  prelude::FredResult,
  runtime::{spawn, BroadcastReceiver, JoinHandle},
  types::{client::Invalidation, MultipleStrings},
};
use fred_macros::rm_send_if;
use futures::Future;

/// A high level interface that supports [client side caching](https://redis.io/docs/manual/client-side-caching/) via the [client tracking](https://redis.io/commands/client-tracking/) interface.
#[cfg_attr(docsrs, doc(cfg(feature = "i-tracking")))]
#[rm_send_if(feature = "glommio")]
pub trait TrackingInterface: ClientLike + Sized {
  /// Send the [CLIENT TRACKING](https://redis.io/commands/client-tracking/) command to all connected servers, subscribing to [invalidation messages](Self::on_invalidation) on the same connection.
  ///
  /// This interface requires the RESP3 protocol mode and supports all server deployment types (centralized,
  /// clustered, and sentinel).
  ///
  /// See the basic [client tracking](crate::interfaces::ClientInterface::client_tracking) function for more
  /// information on the underlying commands.
  fn start_tracking<P>(
    &self,
    prefixes: P,
    bcast: bool,
    optin: bool,
    optout: bool,
    noloop: bool,
  ) -> impl Future<Output = FredResult<()>> + Send
  where
    P: Into<MultipleStrings> + Send,
  {
    async move {
      into!(prefixes);
      commands::tracking::start_tracking(self, prefixes, bcast, optin, optout, noloop).await
    }
  }

  /// Disable client tracking on all connections.
  fn stop_tracking(&self) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::tracking::stop_tracking(self).await }
  }

  /// Spawn a task that processes invalidation messages from the server.
  ///
  /// See [invalidation_rx](Self::invalidation_rx) for a more flexible variation of this function.
  fn on_invalidation<F>(&self, func: F) -> JoinHandle<FredResult<()>>
  where
    F: Fn(Invalidation) -> FredResult<()> + Send + 'static,
  {
    let mut invalidation_rx = self.invalidation_rx();

    spawn(async move {
      let mut result = Ok(());

      while let Ok(invalidation) = invalidation_rx.recv().await {
        if let Err(err) = func(invalidation) {
          result = Err(err);
          break;
        }
      }
      result
    })
  }

  /// Subscribe to invalidation messages from the server(s).
  fn invalidation_rx(&self) -> BroadcastReceiver<Invalidation> {
    self.inner().notifications.invalidations.load().subscribe()
  }
}
