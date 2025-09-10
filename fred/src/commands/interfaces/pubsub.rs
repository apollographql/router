use crate::{
  commands,
  error::Error,
  interfaces::{ClientLike, FredResult},
  types::{FromValue, MultipleStrings, Value},
};
use bytes_utils::Str;
use fred_macros::rm_send_if;
use futures::Future;
use std::convert::TryInto;

/// Functions that implement the [pubsub](https://redis.io/commands#pubsub) interface.
#[rm_send_if(feature = "glommio")]
pub trait PubsubInterface: ClientLike + Sized + Send {
  /// Subscribe to a channel on the publish-subscribe interface.
  ///
  /// <https://redis.io/commands/subscribe>
  fn subscribe<S>(&self, channels: S) -> impl Future<Output = FredResult<()>> + Send
  where
    S: Into<MultipleStrings> + Send,
  {
    async move {
      into!(channels);
      commands::pubsub::subscribe(self, channels).await
    }
  }

  /// Unsubscribe from a channel on the PubSub interface.
  ///
  /// <https://redis.io/commands/unsubscribe>
  fn unsubscribe<S>(&self, channels: S) -> impl Future<Output = FredResult<()>> + Send
  where
    S: Into<MultipleStrings> + Send,
  {
    async move {
      into!(channels);
      commands::pubsub::unsubscribe(self, channels).await
    }
  }

  /// Subscribes the client to the given patterns.
  ///
  /// <https://redis.io/commands/psubscribe>
  fn psubscribe<S>(&self, patterns: S) -> impl Future<Output = FredResult<()>> + Send
  where
    S: Into<MultipleStrings> + Send,
  {
    async move {
      into!(patterns);
      commands::pubsub::psubscribe(self, patterns).await
    }
  }

  /// Unsubscribes the client from the given patterns, or from all of them if none is given.
  ///
  /// If no channels are provided this command returns an empty array.
  ///
  /// <https://redis.io/commands/punsubscribe>
  fn punsubscribe<S>(&self, patterns: S) -> impl Future<Output = FredResult<()>> + Send
  where
    S: Into<MultipleStrings> + Send,
  {
    async move {
      into!(patterns);
      commands::pubsub::punsubscribe(self, patterns).await
    }
  }

  /// Publish a message on the PubSub interface, returning the number of clients that received the message.
  ///
  /// <https://redis.io/commands/publish>
  fn publish<R, S, V>(&self, channel: S, message: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Str> + Send,
    V: TryInto<Value> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(channel);
      try_into!(message);
      commands::pubsub::publish(self, channel, message).await?.convert()
    }
  }

  /// Subscribes the client to the specified shard channels.
  ///
  /// <https://redis.io/commands/ssubscribe/>
  fn ssubscribe<C>(&self, channels: C) -> impl Future<Output = FredResult<()>> + Send
  where
    C: Into<MultipleStrings> + Send,
  {
    async move {
      into!(channels);
      commands::pubsub::ssubscribe(self, channels).await
    }
  }

  /// Unsubscribes the client from the given shard channels, or from all of them if none is given.
  ///
  /// If no channels are provided this command returns an empty array.
  ///
  /// <https://redis.io/commands/sunsubscribe/>
  fn sunsubscribe<C>(&self, channels: C) -> impl Future<Output = FredResult<()>> + Send
  where
    C: Into<MultipleStrings> + Send,
  {
    async move {
      into!(channels);
      commands::pubsub::sunsubscribe(self, channels).await
    }
  }

  /// Posts a message to the given shard channel.
  ///
  /// <https://redis.io/commands/spublish/>
  fn spublish<R, S, V>(&self, channel: S, message: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Str> + Send,
    V: TryInto<Value> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(channel);
      try_into!(message);
      commands::pubsub::spublish(self, channel, message).await?.convert()
    }
  }

  /// Lists the currently active channels.
  ///
  /// <https://redis.io/commands/pubsub-channels/>
  fn pubsub_channels<R, S>(&self, pattern: S) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Str> + Send,
  {
    async move {
      into!(pattern);
      commands::pubsub::pubsub_channels(self, pattern).await?.convert()
    }
  }

  /// Returns the number of unique patterns that are subscribed to by clients.
  ///
  /// <https://redis.io/commands/pubsub-numpat/>
  fn pubsub_numpat<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::pubsub::pubsub_numpat(self).await?.convert() }
  }

  /// Returns the number of subscribers (exclusive of clients subscribed to patterns) for the specified channels.
  ///
  /// <https://redis.io/commands/pubsub-numsub/>
  fn pubsub_numsub<R, S>(&self, channels: S) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<MultipleStrings> + Send,
  {
    async move {
      into!(channels);
      commands::pubsub::pubsub_numsub(self, channels).await?.convert()
    }
  }

  /// Lists the currently active shard channels.
  ///
  /// <https://redis.io/commands/pubsub-shardchannels/>
  fn pubsub_shardchannels<R, S>(&self, pattern: S) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Str> + Send,
  {
    async move {
      into!(pattern);
      commands::pubsub::pubsub_shardchannels(self, pattern).await?.convert()
    }
  }

  /// Returns the number of subscribers for the specified shard channels.
  ///
  /// <https://redis.io/commands/pubsub-shardnumsub/>
  fn pubsub_shardnumsub<R, S>(&self, channels: S) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<MultipleStrings> + Send,
  {
    async move {
      into!(channels);
      commands::pubsub::pubsub_shardnumsub(self, channels).await?.convert()
    }
  }
}
