### Constrain APOLLO_ROUTER_LOG and --log global levels to the router ([Issue #3474](https://github.com/apollographql/router/issues/3474))

`APOLLO_ROUTER_LOG` and `--log` now implicitly set a filter constraining the logging to the `apollo_router` module, simplifying the debugging experience for users.

For advanced users `RUST_LOG` can be used for standard log filter behavior.

Thus:


By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3477