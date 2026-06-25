//! Utilities which make it easy to test with [`crate::plugin`].

mod broken;
mod mock;
mod restricted;
mod service;

#[cfg(test)]
pub use mock::connector::MockConnector;
pub use mock::subgraph::MockSubgraph;

pub(crate) use self::mock::canned;
