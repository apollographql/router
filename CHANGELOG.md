# Changelog

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning v2.0.0](https://semver.org/spec/v2.0.0.html).

# [1.21.0] - 2023-06-20

## 🚀 Features

### Restore HTTP payload size limit, make it configurable ([Issue #2000](https://github.com/apollographql/router/issues/2000))

Early versions of Apollo Router used to rely on a part of the Axum web framework
that imposed a 2 MB limit on the size of the HTTP request body.
Version 1.7 changed to read the body directly, unintentionally removing this limit.

The limit is now restored to help protect against unbounded memory usage, but is now configurable:

```yaml
preview_operation_limits:
  experimental_http_max_request_bytes: 2000000 # Default value: 2 MB
```

This limit is checked while reading from the network, before JSON parsing.
Both the GraphQL document and associated variables count toward it.

Before increasing this limit significantly consider testing performance
in an environment similar to your production, especially if some clients are untrusted.
Many concurrent large requests could cause the Router to run out of memory.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/3130

### Add support for empty auth prefixes ([Issue #2909](https://github.com/apollographql/router/issues/2909))

The `authentication.jwt` plugin now supports empty prefixes for the JWT header. Some companies use prefix-less headers; previously, the authentication plugin rejected requests even with an empty header explicitly set, such as: 

```yml 
authentication:
  jwt:
    header_value_prefix: ""
```

By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/3206

## 🐛 Fixes

### GraphQL introspection errors are now 400 errors ([Issue #3090](https://github.com/apollographql/router/issues/3090))

If we get an introspection error during SupergraphService::plan_query(), then it is reported to the client as an HTTP 500 error. The Router now generates a valid GraphQL error for introspection errors whilst also modifying the HTTP status to be 400.

Before:

StatusCode:500
```json
{"errors":[{"message":"value retrieval failed: introspection error: introspection error : Field \"__schema\" of type \"__Schema!\" must have a selection of subfields. Did you mean \"__schema { ... }\"?","extensions":{"code":"INTERNAL_SERVER_ERROR"}}]}
```

After:

StatusCode:400
```json
{"errors":[{"message":"introspection error : Field \"__schema\" of type \"__Schema!\" must have a selection of subfields. Did you mean \"__schema { ... }\"?","extensions":{"code":"INTROSPECTION_ERROR"}}]}
```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3122

### Restore missing debug tools in "debug" Docker images ([Issue #3249](https://github.com/apollographql/router/issues/3249))

Debug Docker images were designed to make use of `heaptrack` for debugging memory issues. However, this functionality was inadvertently removed when we changed to multi-architecture Docker image builds.

`heaptrack` functionality is now restored to our debug docker images.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3250

### Federation v2.4.8 ([Issue #3217](https://github.com/apollographql/router/issues/3217), [Issue #3227](https://github.com/apollographql/router/issues/3227))

This release bumps the Router's Federation support from v2.4.7 to v2.4.8, which brings in notable query planner fixes from [v2.4.8](https://github.com/apollographql/federation/releases/tag/@apollo/query-planner@2.4.8).  Of note from those releases, this brings query planner fixes that (per that dependency's changelog):

- Fix bug in the handling of dependencies of subgraph fetches. This bug was manifesting itself as an assertion error ([apollographql/federation#2622](https://github.com/apollographql/federation/pull/2622))
thrown during query planning with a message of the form `Root groups X should have no remaining groups unhandled (...)`.

- Fix issues in code to reuse named fragments. One of the fixed issue would manifest as an assertion error with a message ([apollographql/federation#2619](https://github.com/apollographql/federation/pull/2619))
looking like `Cannot add fragment of condition X (...) to parent type Y (...)`. Another would manifest itself by
generating an invalid subgraph fetch where a field conflicts with another version of that field that is in a reused
named fragment.

These manifested as Router issues https://github.com/apollographql/router/issues/3217 and https://github.com/apollographql/router/issues/3227.

By [@renovate](https://github.com/renovate) and [o0ignition0o](https://github.com/o0ignition0o) in https://github.com/apollographql/router/pull/3202

### update Rhai to 1.15.0 to fix issue with hanging example test ([Issue #3213](https://github.com/apollographql/router/issues/3213))

One of our Rhai examples' tests have been regularly hanging in the CI builds. Investigation uncovered a race condition within Rhai itself. This update brings in the fixed version of Rhai and should eliminate the hanging problem and improve build stability.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3273

## 🛠 Maintenance

### chore: split out router events into its own module ([PR #3235](https://github.com/apollographql/router/pull/3235))

Breaks down `./apollo-router/src/router.rs` into its own module `./apollo-router/src/router/mod.rs` with a sub-module `./apollo-router/src/router/event/mod.rs` that contains all the streams that we combine to start a router (entitlement, schema, reload, configuration, shutdown, more streams to be added).

By [@EverlastingBugstopper](https://github.com/EverlastingBugstopper) in https://github.com/apollographql/router/pull/3235

### Simplify router service tests ([PR #3259](https://github.com/apollographql/router/pull/3259))

Parts of the router service creation were generic, to allow mocking, but the `TestHarness` API allows us to reuse the same code in all cases. Generic types have been removed to simplify the API.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3259

## 📚 Documentation

### Improve example Rhai scripts for JWT Authentication ([PR #3184](https://github.com/apollographql/router/pull/3184))

Simplify the example Rhai scripts in the [JWT Authentication](https://www.apollographql.com/docs/router/configuration/authn-jwt) docs and includes a sample `main.rhai` file to make it clear how to use all scripts together.

By [@dbanty](https://github.com/dbanty) in https://github.com/apollographql/router/pull/3184

## 🧪 Experimental

### Expose the apollo compiler at the supergraph service level (internal) ([PR #3200](https://github.com/apollographql/router/pull/3200))

Add a query analysis phase inside the router service, before sending the query through the supergraph plugins. It makes a compiler available to supergraph plugins, to perform deeper analysis of the query. That compiler is then used in the query planner to create the `Query` object containing selections for response formatting.

This is for internal use only for now, and the APIs are not considered stable.

By [@o0Ignition0o](https://github.com/o0Ignition0o) and [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3200

### Query planner plugins (internal) ([Issue #3150](https://github.com/apollographql/router/issues/3150))

Future functionality may need to modify a query between query plan caching and the query planner. This leads to the requirement to provide a query planner plugin capability.

Query planner plugin functionality exposes an ApolloCompiler instance to perform preprocessing of a query before sending it to the query planner.

This is for internal use only for now, and the APIs are not considered stable.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3177 and https://github.com/apollographql/router/pull/3252
