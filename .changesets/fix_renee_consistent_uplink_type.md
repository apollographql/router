### Fix inconsistent `type` attribute in `apollo.router.uplink.fetch.duration` metric ([PR #5816](https://github.com/apollographql/router/pull/5816))

The router now always reports a short name in the `type` attribute for the `apollo.router.fetch.duration` metric, instead of sometimes using a fully-qualified Rust path and sometimes using a short name.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/5816
