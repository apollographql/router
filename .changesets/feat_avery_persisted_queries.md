### feat: `preview_persisted_queries` w/opt-in safelisting ([PR #3347](https://github.com/apollographql/router/pull/3347))

# Persisted Queries

> ⚠️ **This is an [Enterprise feature](https://www.apollographql.com/blog/platform/evaluating-apollo-router-understanding-free-and-open-vs-commercial-features/) of the Apollo Router.** It requires an organization with a [GraphOS Enterprise plan](https://www.apollographql.com/pricing/) and the feature to be enabled for your account.
>
> If your organization _doesn't_ currently have an Enterprise plan, you can test out this functionality by signing up for a free [Enterprise trial](https://www.apollographql.com/docs/graphos/org/plans/#enterprise-trials) and reaching out to enable the feature for your account.

## Overview

The persisted queries feature allows you to pre-register operations, allowing clients to send an operation ID over the wire and execute the associated operation. Each operation defines the exact shape of a GraphQL operation that the router expects clients to send. In its simplest form, Persisted Queries (PQ’s) can be used like Automatic Persisted Queries (APQ’s) with one key difference: sending an operation body is never allowed for a PQ. Registering persisted operations allows locking down the router to log unregistered operations, or to reject them outright.

### Main Configurations

* **Unregistered operation monitoring**
  * Your router can allow all GraphQL operations, while emitting structured traces containing unregistered operation bodies.
* **Operation safelisting**
  * Reject unregistered operations
  * Require all operations to be sent as an ID

## Usage

```yaml title="router.yaml"
preview_persisted_queries:
  enabled: true
```

This enables additive PQs.

Requires `APOLLO_KEY` and `APOLLO_GRAPH_REF` to start up properly (to fetch the license key and the persisted queries themselves), and the graph variant must be linked to a persisted query list. This is only available in preview right now and has to be enabled for a graph.

To create a persisted query list and link it to your graph, see our [mock docs](https://docs.google.com/document/d/16EcmcbjmwLfDfAhpMWdF9bHPG8kZ38htXKL-ozVPOUQ/edit#heading=h.r8r7mfcvvw4f), it walks you through enabling the preview feature for your graph, creating a persisted query list, and publishing operations to it from Rover.

The router will not start up until all persisted queries have been read into a `std::collections::HashMap<String, String>` mapping ID to their body. Additionally, just the bodies are stored in a `std::collections::HashSet`.

After the router starts, persisted queries can be sent over the wire like so:

```sh
curl http://localhost:4000/ -X POST --json \
'{"extensions":{"persistedQuery":{"version":1,"sha256Hash":"dc67510fb4289672bea757e862d6b00e83db5d3cbbcfb15260601b6f29bb2b8f"}}}'
```

2) [./examples/persisted-queries/safelist_pq_log_only.yaml](https://github.com/apollographql/router/raw/avery/persisted-queries/examples/persisted-queries/safelist_pq_log_only.yaml)

```yaml title="router.yaml"
preview_persisted_queries:
  enabled: true
  log_unpersisted_queries: true
```

Starting the router with this configuration logs freeform GraphQL operations that do not match a persisted query.

3) [./examples/persisted-queries/safelist_pq.yaml](https://github.com/apollographql/router/raw/avery/persisted-queries/examples/persisted-queries/safelist_pq.yaml)

```yaml title="router.yaml"
preview_persisted_queries:
  enabled: true
  safelist:
    enabled: true
apq:
  enabled: false
```

Starting the router with this configuration will require all operations sent over the wire to match either the ID (O(1) retrieval from `HashMap`) or the body (O(1) retrieval from `HashSet`). APQ is enabled by default, and is incompatible with the persisted queries feature (clients are not allowed to register their own persisted queries, they must be pre-published), therefore it must be disabled to start properly. An error is returned if APQ is not explicitly disabled in `router.yaml`.

4) [./examples/persisted-queries/safelist_pq_require_id.yaml](https://github.com/apollographql/router/raw/avery/persisted-queries/examples/persisted-queries/safelist_pq_require_id.yaml)

```yaml title="router.yaml"
preview_persisted_queries:
  enabled: true
  safelist:
    enabled: true
    require_id: true
apq:
  enabled: false
```

This configuration is a stricter version of safelisting that rejects all freeform GraphQL requests, even if they match the body of a persisted query.

By [@EverlastingBugstopper](https://github.com/EverlastingBugstopper) in https://github.com/apollographql/router/pull/3347