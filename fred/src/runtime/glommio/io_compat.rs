//! Reuse the same approach used by gmf (https://github.com/EtaCassiopeia/gmf/blob/591037476e6a17f83954a20558ff0e1920d94301/gmf/src/server/tokio_interop.rs#L1).
//!
//! The `Framed<T, U>` codec interface used by the `Connection` struct requires that `T: AsyncRead+AsyncWrite`.
//! These traits are defined in the tokio and futures_io/futures_lite crates, but the tokio_util::codec interface
//! uses the versions re-implemented in tokio. However, glommio's network interfaces implement
//! `AsyncRead+AsyncWrite` from the futures_io crate. There are several ways to work around this, including
//! either a re-implementation of the codec traits `Encoder+Decoder`, or a compatibility layer for the different
//! versions of `AsyncRead+AsyncWrite`. The `gmf` project used the second approach, which seems much easier than
//! re-implementing the `Framed` traits (https://github.com/tokio-rs/tokio/blob/1ac8dff213937088616dc84de9adc92b4b68c49a/tokio-util/src/codec/framed_impl.rs#L125).

// ------------------- https://github.com/EtaCassiopeia/gmf/blob/591037476e6a17f83954a20558ff0e1920d94301/gmf/src/server/tokio_interop.rs

/// This module provides interoperability with the Tokio async runtime. It contains utilities to bridge between
/// futures_lite and Tokio.
use std::io::{self};
use std::{
  pin::Pin,
  task::{Context, Poll},
};

use futures_io::{AsyncRead, AsyncWrite};
use tokio::io::ReadBuf;

/// A wrapper type for AsyncRead + AsyncWrite + Unpin types, providing interoperability with Tokio's AsyncRead and
/// AsyncWrite traits.
#[pin_project::pin_project] // This generates a projection for the inner type.
pub struct TokioIO<T>(#[pin] pub T)
where
  T: AsyncRead + AsyncWrite + Unpin;

impl<T> tokio::io::AsyncWrite for TokioIO<T>
where
  T: AsyncRead + AsyncWrite + Unpin,
{
  /// Write some data into the inner type, returning how many bytes were written.
  fn poll_write(self: Pin<&mut Self>, cx: &mut Context, buf: &[u8]) -> Poll<io::Result<usize>> {
    // This is the same as  Pin::new(&mut self.0).poll_write(cx, buf) with the source type of `mut self`
    // using projection makes it easier to read.
    let this = self.project();
    this.0.poll_write(cx, buf)
  }

  /// Flushes the inner type.
  fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
    self.project().0.poll_flush(cx)
  }

  /// Shuts down the inner type, flushing any buffered data.
  fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
    self.project().0.poll_close(cx)
  }
}

impl<T> tokio::io::AsyncRead for TokioIO<T>
where
  T: AsyncRead + AsyncWrite + Unpin,
{
  /// Reads some data from the inner type, returning how many bytes were read.
  fn poll_read(self: Pin<&mut Self>, cx: &mut Context, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
    self.project().0.poll_read(cx, buf.initialize_unfilled()).map(|n| {
      if let Ok(n) = n {
        buf.advance(n);
      }

      Ok(())
    })
  }
}
