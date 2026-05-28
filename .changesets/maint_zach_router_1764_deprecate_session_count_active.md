### Deprecate `apollo.router.session.count.active` metric

The `apollo.router.session.count.active` up/down counter is now marked deprecated in its exported metric description, directing operators to the OpenTelemetry-compliant replacement `http.server.active_requests`. The metric continues to be emitted under its current name for backward compatibility, but may be removed in a future release.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/XXXX
