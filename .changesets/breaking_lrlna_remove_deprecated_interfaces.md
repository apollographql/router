### Remove deprecated interfaces and metrics ([PR #6646](https://github.com/apollographql/router/pull/6646))

#### Rust 
The following deprecated methods are removed from rust plugins' API:

- `services::router::Response::map()`
- `router::event::schema::SchemaSource::File.delay` field
- `router::event::configuration::Configuration::File.delay` field
- `context::extensions::sync::ExtensionsMutex::lock()`. Use `ExtensionsMutex::with_lock()` instead.
- `test_harness::TestHarness::build()`. Use `TestHarness::build_supergraph()` instead.
- `PluginInit::new()`. Use `PluginInit::builder()` instead.
- `PluginInit::try_new()`. Use `PluginInit::try_builder()` instead.

#### Metrics
The following deprecated metrics are removed: 
- `apollo_require_authentication_failure_count. Use the
`http.server.request.duration` metric's `http.response.status_code` attribute.
Requests with authentication failures have HTTP status code 401.
- `apollo_authentication_failure_count`. Use the
`apollo.router.operations.authentication.jwt` metric's
`authentication.jwt.failed` attribute.
- `apollo_authentication_success_count`. Use the
`apollo.router.operations.authentication.jwt` metric instead. If the
`authentication.jwt.failed` attribute is _absent_ or `false`, the authentication
succeeded.
- `apollo_router_deduplicated_subscriptions_total`. Use the
`apollo.router.operations.subscriptions` metric's `subscriptions.deduplicated`
attribute
- Calculating the overhead of injecting the router into your service stack when
  making multiple downstream calls is a complex task. With that, we are removing
  the following two misleading metrics and recommending that you instead test your
  workloads with the router to if the latency meets your requirements:
  - `apollo_router_span`

#### CLI
The deprecated `--schema` command-line argument is removed. `router config schema` should be used to print the configuration supergraph instead.

By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/6646