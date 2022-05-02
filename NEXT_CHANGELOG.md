# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <THIS IS AN EXAMPLE, DO NOT REMOVE>

# [x.x.x] (unreleased) - 2022-mm-dd
> Important: X breaking changes below, indicated by **❗ BREAKING ❗**
## ❗ BREAKING ❗
## 🚀 Features ( :rocket: )
## 🐛 Fixes ( :bug: )
## 🛠 Maintenance ( :hammer_and_wrench: )
## 📚 Documentation ( :books: )
## 🐛 Fixes ( :bug: )

## Example section entry format

- **Headline** ([PR #PR_NUMBER](https://github.com/apollographql/router/pull/PR_NUMBER))

  Description! And a link to a [reference](http://url)
-->

# [v0.1.0-preview.7] - (unreleased)
## ❗ BREAKING ❗

### Plugin utilities cleanup ([PR #819](https://github.com/apollographql/router/pull/819)) ([PR #908](https://github.com/apollographql/router/pull/908))
Utilities around creating Request and Response structures have been migrated to builders.

Migration:
* `plugin_utils::RouterRequest::builder()`->`RouterRequest::fake_builder()`
* `plugin_utils::RouterResponse::builder()`->`RouterResponse::fake_builder()`

In addition, the `plugin_utils` module has been removed. Mock service functionality has been migrated to `plugin::utils::test`.
### Plugin API changes [PR #855](https://github.com/apollographql/router/pull/855)
Previously the Plugin trait has three lifecycle hooks: new, startup, and shutdown.

Startup and shutdown are problematic because:
* Plugin construction happens in new and startup. This means creating in new and populating in startup.
* Startup and shutdown has to be explained to the user.
* Startup and shutdown ordering is delicate.

The lifecycle now looks like this:
1. `new`
2. `activate`
3. `drop`

Users can migrate their plugins using the following:
* `Plugin#startup`->`Plugin#new`
* `Plugin#shutdown`->`Drop#drop`

In addition, the `activate` lifecycle hook is now not marked as deprecated, and users are free to use it.

## 🚀 Features

### Add SpanKind and SpanStatusCode to follow the opentelemetry spec [PR #925](https://github.com/apollographql/router/pull/925)
Spans now contains [`otel.kind`](https://github.com/open-telemetry/opentelemetry-specification/blob/main/specification/trace/api.md#spankind) and [`otel.status_code`](https://github.com/open-telemetry/opentelemetry-specification/blob/main/specification/trace/api.md#set-status) attributes when needed to follow the opentelemtry spec .

###  Configurable client identification headers [PR #850](https://github.com/apollographql/router/pull/850)
The router uses the HTTP headers `apollographql-client-name` and `apollographql-client-version` to identify clients in Studio telemetry. Those headers can now be overriden in the configuration:
```yaml title="router.yaml"
telemetry:
  apollo:
    # Header identifying the client name. defaults to apollographql-client-name
    client_name_header: <custom_client_header_name>
    # Header identifying the client version. defaults to apollographql-client-version
    client_version_header: <custom_version_header_name>
```

## 🐛 Fixes
### Fields in the root selection set of a query are now correctly skipped and included [PR #931](https://github.com/apollographql/router/pull/931)
The `@skip` and `@include` directives are now executed for the fields in the root selection set.

### Configuration errors on hot-reload are output [PR #850](https://github.com/apollographql/router/pull/850)
If a configuration file had errors on reload these were silently swallowed. These are now added to the logs.

### Telemetry spans are no longer created for healthcheck requests [PR #938](https://github.com/apollographql/router/pull/938)
Telemetry spans where previously being created for the healthcheck requests which was creating noisy telemetry for users.

### Dockerfile now allows overriding of `CONFIGURATION_PATH` [PR #948](https://github.com/apollographql/router/pull/948)
Previously `CONFIGURATION_PATH` could not be used to override the config location as it was being passed by command line arg. 

## 🛠 Maintenance
### Upgrade `test-span` to display more children spans in our snapshots [PR #942](https://github.com/apollographql/router/pull/942)
Previously in test-span before the fix [introduced here](https://github.com/apollographql/test-span/pull/13) we were filtering too aggressively. So if we wanted to snapshot all `DEBUG` level if we encountered a `TRACE` span which had `DEBUG` children then these children were not snapshotted. It's now fixed and it's more consistent with what we could have/see in jaeger.

### Finalize migration from Warp to Axum [PR #920](https://github.com/apollographql/router/pull/920)
Adding more tests to be more confident to definitely delete the `warp-server` feature and get rid of `warp`

### End to end integration tests for Jaeger [PR #850](https://github.com/apollographql/router/pull/850)
Jaeger tracing end to end test including client->router->subgraphs

### Router tracing span cleanup [PR #850](https://github.com/apollographql/router/pull/850)
Spans generated by the Router are now aligned with plugin services.

### Simplified CI for windows [PR #850](https://github.com/apollographql/router/pull/850)
All windows processes are spawned via xtask rather than a separate CircleCI stage.

### Enable default feature in graphql_client [PR #905](https://github.com/apollographql/router/pull/905)
Removing the default feature can cause build issues in plugins.

### Do not remove __typename from the aggregated response [PR #919](https://github.com/apollographql/router/pull/919)
If the client was explicitely requesting the `__typename` field, it was removed from the aggregated subgraph data, and so was not usable by fragment to check the type.

### Follow the GraphQL spec about Response format [PR #926](https://github.com/apollographql/router/pull/926)
The response's `data` field can be null or absent depending on conventions that are now followed by the router.

## Add client awareness headers to CORS allowed headers [PR #917](https://github.com/apollographql/router/pull/917)

The client awareness headers are now added by default to the list of CORS allowed headers, for easier integration of browser based applications. We also document how to override them and update the CORS configuration accordingly.

## Remove unnecessary box in instrumentation layer [PR #940](https://github.com/apollographql/router/pull/940)

Minor simplification of code to remove boxing during instrumentation.

## 📚 Documentation
### Enhanced rust docs ([PR #819](https://github.com/apollographql/router/pull/819))
Many more rust docs have been added.

### Federation version support page [PR #896](https://github.com/apollographql/router/pull/896)
Add Federation version support doc page detailing which versions of federation are compiled against versions of the router.

### Improve readme for embedded Router [PR #936](https://github.com/apollographql/router/pull/936)
Add more details about pros and cons so that users know what they're letting themselves in for.  
