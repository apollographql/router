//! Whether the client that sent a request is still there to receive the response.

use std::sync::Arc;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;

use http::StatusCode;

/// 499 Client Closed Request. HTTP does not define it, but it is the common status for a request
/// whose client left before the response was ready.
pub(crate) fn client_closed_request() -> StatusCode {
    StatusCode::from_u16(499).expect("499 is in the valid status code range")
}

// The default, 0, means the client is still connected.
const CLOSED: u8 = 1;

/// The state of the client connection a request arrived on.
///
/// It is stored in the request's [`Context`], and every clone shares the same state, so work that
/// carries on after the client leaves can see that nobody is waiting for its answer. A request
/// whose context has none is treated as connected.
#[derive(Clone, Debug, Default)]
pub(crate) struct ClientConnection {
    state: Arc<AtomicU8>,
}

impl ClientConnection {
    /// Records that the client closed the connection before it was answered.
    pub(crate) fn close(&self) {
        self.state.store(CLOSED, Ordering::Release);
    }

    /// Returns `true` once the client has closed the connection.
    pub(crate) fn is_closed(&self) -> bool {
        self.state.load(Ordering::Acquire) == CLOSED
    }
}
