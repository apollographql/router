use crate::interfaces::ClientLike;

#[cfg(feature = "metrics")]
use crate::modules::metrics::Stats;

/// Functions that implement the internal metrics interface.
pub trait MetricsInterface: ClientLike + Sized {
  /// Read the number of request redeliveries.
  ///
  /// This is the number of times a request had to be sent again due to a connection closing while waiting on a
  /// response.
  fn read_redelivery_count(&self) -> usize {
    self.inner().counters.read_redelivery_count()
  }

  /// Read and reset the number of request redeliveries.
  fn take_redelivery_count(&self) -> usize {
    self.inner().counters.take_redelivery_count()
  }

  /// Read the number of buffered commands that have not yet been sent to the server.
  fn command_queue_len(&self) -> usize {
    self.inner().counters.read_cmd_buffer_len()
  }

  /// Read latency metrics across all commands.
  ///
  /// This metric reflects the total latency experienced by callers, including time spent waiting in memory to be
  /// written and network latency. Features such as automatic reconnect and frame serialization time can all affect
  /// these values.
  #[cfg(feature = "metrics")]
  #[cfg_attr(docsrs, doc(cfg(feature = "metrics")))]
  fn read_latency_metrics(&self) -> Stats {
    self.inner().latency_stats.read().read_metrics()
  }

  /// Read and consume latency metrics, resetting their values afterwards.
  #[cfg(feature = "metrics")]
  #[cfg_attr(docsrs, doc(cfg(feature = "metrics")))]
  fn take_latency_metrics(&self) -> Stats {
    self.inner().latency_stats.write().take_metrics()
  }

  /// Read network latency metrics across all commands.
  ///
  /// This metric only reflects time spent waiting on a response. It will factor in reconnect time if a response
  /// doesn't arrive due to a connection closing, but it does not include the time a command spends waiting to be
  /// written, serialization time, backpressure, etc.
  #[cfg(feature = "metrics")]
  #[cfg_attr(docsrs, doc(cfg(feature = "metrics")))]
  fn read_network_latency_metrics(&self) -> Stats {
    self.inner().network_latency_stats.read().read_metrics()
  }

  /// Read and consume network latency metrics, resetting their values afterwards.
  #[cfg(feature = "metrics")]
  #[cfg_attr(docsrs, doc(cfg(feature = "metrics")))]
  fn take_network_latency_metrics(&self) -> Stats {
    self.inner().network_latency_stats.write().take_metrics()
  }

  /// Read request payload size metrics across all commands.
  #[cfg(feature = "metrics")]
  #[cfg_attr(docsrs, doc(cfg(feature = "metrics")))]
  fn read_req_size_metrics(&self) -> Stats {
    self.inner().req_size_stats.read().read_metrics()
  }

  /// Read and consume request payload size metrics, resetting their values afterwards.
  #[cfg(feature = "metrics")]
  #[cfg_attr(docsrs, doc(cfg(feature = "metrics")))]
  fn take_req_size_metrics(&self) -> Stats {
    self.inner().req_size_stats.write().take_metrics()
  }

  /// Read response payload size metrics across all commands.
  #[cfg(feature = "metrics")]
  #[cfg_attr(docsrs, doc(cfg(feature = "metrics")))]
  fn read_res_size_metrics(&self) -> Stats {
    self.inner().res_size_stats.read().read_metrics()
  }

  /// Read and consume response payload size metrics, resetting their values afterwards.
  #[cfg(feature = "metrics")]
  #[cfg_attr(docsrs, doc(cfg(feature = "metrics")))]
  fn take_res_size_metrics(&self) -> Stats {
    self.inner().res_size_stats.write().take_metrics()
  }
}
