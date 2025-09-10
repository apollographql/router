use crate::{
  commands,
  interfaces::{ClientLike, FredResult},
  prelude::FromValue,
  types::Key,
};
use fred_macros::rm_send_if;
use futures::Future;

/// Functions that implement the [memory](https://redis.io/commands#server) interface.
#[rm_send_if(feature = "glommio")]
pub trait MemoryInterface: ClientLike + Sized {
  /// The MEMORY DOCTOR command reports about different memory-related issues that the Redis server experiences, and
  /// advises about possible remedies.
  ///
  /// <https://redis.io/commands/memory-doctor>
  fn memory_doctor<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::memory::memory_doctor(self).await?.convert() }
  }

  /// The MEMORY MALLOC-STATS command provides an internal statistics report from the memory allocator.
  ///
  /// <https://redis.io/commands/memory-malloc-stats>
  fn memory_malloc_stats<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::memory::memory_malloc_stats(self).await?.convert() }
  }

  /// The MEMORY PURGE command attempts to purge dirty pages so these can be reclaimed by the allocator.
  ///
  /// <https://redis.io/commands/memory-purge>
  fn memory_purge(&self) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::memory::memory_purge(self).await }
  }

  /// The MEMORY STATS command returns an Array reply about the memory usage of the server.
  ///
  /// <https://redis.io/commands/memory-stats>
  fn memory_stats<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::memory::memory_stats(self).await?.convert() }
  }

  /// The MEMORY USAGE command reports the number of bytes that a key and its value require to be stored in RAM.
  ///
  /// <https://redis.io/commands/memory-usage>
  fn memory_usage<R, K>(&self, key: K, samples: Option<u32>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
  {
    async move {
      into!(key);
      commands::memory::memory_usage(self, key, samples).await?.convert()
    }
  }
}
