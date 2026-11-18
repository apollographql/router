### Remove unused `connection_closed_signal` from `subgraph::Request` ([PR #10773](https://github.com/apollographql/router/pull/10773))

This subgraph request property was never meant to be public. You can no longer set the `connection_closed_signal` property on a subgraph request builder in Rust plugins.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/10773