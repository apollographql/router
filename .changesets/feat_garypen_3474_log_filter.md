### Constrain APOLLO_ROUTER_LOG and --log global levels to the router ([Issue #3474](https://github.com/apollographql/router/issues/3474))

It would be nice if users could specify just a log level and the router applied the required filtering to constrain the logging to the `apollo_router` module.

It would also be nice if, for advanced users, you could exercise the full power of a logging filter.

This PR enables both these use cases.

If you set a filter using `RUST_LOG`, it is used as is.

If you set it using `APOLLO_ROUTER_LOG` or `--log`, then any "global" scope levels are constrained to `apollo_router`.

Thus:

```
RUST_LOG=apollo_router=warn
--log warn
APOLLO_ROUTER_LOG=warn
```

are equivalent with all three statements resulting in `warn` level logging for the router.

For more details, read the logging configuration documentation.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3477