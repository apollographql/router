#[cfg(any(test, feature = "mock_subgraphs_testing"))]
pub(crate) mod canned;
#[cfg(test)]
pub(super) mod connector;
pub(super) mod subgraph;
