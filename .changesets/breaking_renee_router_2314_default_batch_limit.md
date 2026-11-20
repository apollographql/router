### Limit client batches to 10 operations by default ([PR #10851](https://github.com/apollographql/router/pull/10851))

When query batching is enabled, `batching.maximum_size` now defaults to `10` and can no longer be turned off. Previously, a router without `maximum_size` accepted batches of any size.

If your clients send larger batches, check the `apollo.router.operations.batching.size` metric and set `maximum_size` a little above the largest batch you see.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/10851
