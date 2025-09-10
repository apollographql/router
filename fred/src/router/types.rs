use crate::{
  error::{Error, ErrorKind},
  modules::inner::ClientInner,
  protocol::{connection::Connection, types::Server},
  runtime::RefCount,
  types::Resp3Frame,
};
use futures::stream::Stream;
use std::{
  collections::HashMap,
  future::Future,
  pin::Pin,
  task::{Context, Poll},
  time::Instant,
};

/// Options describing how to change connections in a cluster.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClusterChange {
  pub add:    Vec<Server>,
  pub remove: Vec<Server>,
}

impl Default for ClusterChange {
  fn default() -> Self {
    ClusterChange {
      add:    Vec::new(),
      remove: Vec::new(),
    }
  }
}

// The following future types are used in the context of a select! loop, so they return Pending when there are no
// available connections to poll.

fn poll_connection(
  inner: &RefCount<ClientInner>,
  conn: &mut Connection,
  cx: &mut Context<'_>,
  buf: &mut Vec<(Server, Option<Result<Resp3Frame, Error>>)>,
  now: &Instant,
) {
  match Pin::new(&mut conn.transport).poll_next(cx) {
    Poll::Ready(Some(frame)) => {
      conn.last_write = None;
      buf.push((conn.server.clone(), Some(frame.map(|f| f.into_resp3()))));
    },
    Poll::Ready(None) => {
      conn.last_write = None;
      buf.push((conn.server.clone(), None));
    },
    Poll::Pending => {
      if let Some(duration) = inner.connection.unresponsive.max_timeout {
        if let Some(last_write) = conn.last_write {
          if now.saturating_duration_since(last_write) > duration && !conn.blocked {
            buf.push((
              conn.server.clone(),
              Some(Err(Error::new(ErrorKind::IO, "Unresponsive connection."))),
            ));
          }
        }
      }
    },
  };
}

/// A future that reads from all connections and performs unresponsive checks.
// `poll_next` on a Framed<TcpStream> is not cancel-safe
pub struct ReadAllFuture<'a, 'b> {
  inner:       &'a RefCount<ClientInner>,
  connections: &'b mut HashMap<Server, Connection>,
  #[cfg(feature = "replicas")]
  replicas:    &'b mut HashMap<Server, Connection>,
}

impl<'a, 'b> ReadAllFuture<'a, 'b> {
  #[cfg(not(feature = "replicas"))]
  pub fn new(inner: &'a RefCount<ClientInner>, connections: &'b mut HashMap<Server, Connection>) -> Self {
    Self { connections, inner }
  }

  #[cfg(feature = "replicas")]
  pub fn new(
    inner: &'a RefCount<ClientInner>,
    connections: &'b mut HashMap<Server, Connection>,
    replicas: &'b mut HashMap<Server, Connection>,
  ) -> Self {
    Self {
      connections,
      inner,
      replicas,
    }
  }
}

impl Future for ReadAllFuture<'_, '_> {
  type Output = Vec<(Server, Option<Result<Resp3Frame, Error>>)>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    #[cfg(feature = "replicas")]
    if self.connections.is_empty() && self.replicas.is_empty() {
      return Poll::Pending;
    }
    #[cfg(not(feature = "replicas"))]
    if self.connections.is_empty() {
      return Poll::Pending;
    }

    let _self = self.get_mut();
    let now = Instant::now();
    let mut out = Vec::new();
    for (_, conn) in _self.connections.iter_mut() {
      poll_connection(_self.inner, conn, cx, &mut out, &now);
    }
    #[cfg(feature = "replicas")]
    for (_, conn) in _self.replicas.iter_mut() {
      poll_connection(_self.inner, conn, cx, &mut out, &now);
    }

    if out.is_empty() {
      Poll::Pending
    } else {
      Poll::Ready(out)
    }
  }
}

/// A future that reads from the connection and performs unresponsive checks.
pub struct ReadFuture<'a, 'b> {
  inner:      &'a RefCount<ClientInner>,
  connection: &'b mut Connection,
  #[cfg(feature = "replicas")]
  replicas:   &'b mut HashMap<Server, Connection>,
}

impl<'a, 'b> ReadFuture<'a, 'b> {
  #[cfg(not(feature = "replicas"))]
  pub fn new(inner: &'a RefCount<ClientInner>, connection: &'b mut Connection) -> Self {
    Self { connection, inner }
  }

  #[cfg(feature = "replicas")]
  pub fn new(
    inner: &'a RefCount<ClientInner>,
    connection: &'b mut Connection,
    replicas: &'b mut HashMap<Server, Connection>,
  ) -> Self {
    Self {
      inner,
      connection,
      replicas,
    }
  }
}

impl Future for ReadFuture<'_, '_> {
  type Output = Vec<(Server, Option<Result<Resp3Frame, Error>>)>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let mut out = Vec::new();
    let now = Instant::now();
    let _self = self.get_mut();

    poll_connection(_self.inner, _self.connection, cx, &mut out, &now);
    #[cfg(feature = "replicas")]
    for (_, conn) in _self.replicas.iter_mut() {
      poll_connection(_self.inner, conn, cx, &mut out, &now);
    }

    if out.is_empty() {
      Poll::Pending
    } else {
      Poll::Ready(out)
    }
  }
}
