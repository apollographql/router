# Provider-neutral event streams for federated subscriptions

## Status

Proposed

## Context

Federated subscriptions currently obtain their primary event by subscribing to a
subgraph over WebSocket or callback, then execute the remainder of the query plan
for every event. Some graphs instead publish entity-shaped events to systems such
as NATS, Kafka, Redis Streams, cloud pub/sub services, or an internal event system.

The Router needs an event-backed primary source without coupling graph metadata,
Router configuration, or federated execution to one provider. The initial public
configuration needs room to grow because moving or renaming fields later is a
breaking change. Internal Rust contracts can evolve with the implementation and do
not need to be fixed before the first provider-backed vertical slice.

## Decision

### Use three stable configuration namespaces

The initial configuration version is implicit. There is deliberately no `version`
field.

```yaml
events:
  providers:
    production-events:
      type: nats
      config:
        servers: [nats://localhost:4222]
      lifecycle:
        connect_timeout: 10s

  sources:
    product-updates:
      provider: production-events
      policy: live-updates
      format:
        type: graphql_entity
        config: {}
      provider_options: {}

  policies:
    live-updates:
      delivery:
        type: live
        start: { type: latest }
        acknowledgement: { type: on_enqueue }
      buffer:
        capacity: 128
        overflow: { type: drop_oldest }
      distribution: { type: every_router_instance }
      ordering: { type: provider }
```

- `providers` owns physical connections and credentials.
- `sources` gives composed graph metadata a provider-independent logical name.
- `policies` owns reusable, externally observable delivery semantics.

Provider-specific connection settings live only in `providers.*.config`.
Provider-specific consumer settings live only in
`sources.*.provider_options`. Format-specific mappings live only in
`sources.*.format.config`. These extension points are always objects and are
must be validated by their selected implementation.

Common behavior uses tagged objects (`type: ...`) instead of booleans. New behavior
can therefore be introduced by adding variants without moving existing fields or
changing a scalar into an object.

Local trigger sharing is an implementation detail, not public configuration. A
future user-visible control should be added only when it has a concrete use case.
Likewise, additional startup behavior can be added as an optional lifecycle field
when an adapter implements it; omitting it continues to mean startup is required.

### Keep graph metadata logical

Composition should expose a logical source and provider-neutral destinations, for
example:

```graphql
type Subscription {
  productUpdated: Product!
    @event__subscribe(source: "product-updates", destinations: ["products.updated"])
}
```

The Router config maps `product-updates` to a provider. A graph must not contain a
broker URL, topic implementation type, consumer group, credentials, or delivery
policy.

### Add runtime behavior as a vertical slice

The first runtime PR should include composed-schema extraction, decoding, one
production provider adapter, and integration with existing federated subscription
execution. This avoids committing speculative internal traits or changing the
compatibility-sensitive subscription channel before an event source uses it.

The intended flow is:

```text
graph subscription metadata
          |
          v
logical source -> provider adapter -> provider-neutral event
                                      -> format decoder
                                      -> federated hydration
                                      -> client stream
```

The provider-neutral event must retain its raw payload, metadata, opaque cursor,
and acknowledgement handle until the configured acknowledgement point. Equivalent
triggers may share one provider subscription inside a Router process, but trigger
identity must include every input that affects event selection or security.

Each adapter must map common policies onto native topology. In particular,
`every_router_instance` means every live Router process receives every matching
event. A Kafka adapter might use an instance-unique consumer group, while a cloud
pub/sub adapter must not attach all Router processes to one load-balanced
subscription.

## Compatibility rules

The following configuration is treated as stable from the first public release:

- The `events.providers`, `events.sources`, and `events.policies` separation.
- Logical source names in composed graph metadata.
- The `type` discriminator on providers, formats, and policy variants.
- Delegated object-valued `config` and `provider_options` extension points.
- Explicit delivery, acknowledgement, start, buffering, distribution, and ordering
  semantics.

New provider and policy variants may be added. Existing variants must retain their
meaning. If a future design cannot be expressed by adding a tagged variant or an
optional field, it requires an explicit migration rather than silently changing
semantics. An explicit top-level feature version should be introduced only when a
second incompatible configuration grammar is actually needed.

Internal provider traits, trigger keys, health state, shutdown behavior, and
subscription envelopes are deliberately not part of this compatibility promise.

## First-pass scope

This contract-first change establishes and locally tests:

- implicit-version configuration and unknown-field rejection;
- stable provider, source, and policy namespaces;
- object-valued provider and format extension points;
- cross-reference and name validation;
- generated Router configuration schema coverage.

The provider-backed vertical slice must add:

1. composed-schema directive extraction and validation;
2. a minimal provider interface and the `graphql_entity` decoder;
3. one production provider adapter plus topology conformance tests;
4. trigger deduplication and bounded local fan-out;
5. lifecycle ownership, health reporting, and shutdown;
6. metrics and traces for connections, triggers, lag, drops, decoding,
   acknowledgements, and hydration;
7. end-to-end tests for reconnects, duplicate delivery, slow clients, reloads, and
   Router shutdown.

## Consequences

The public configuration can be reviewed independently from provider mechanics,
and the next PR can choose the smallest internal API supported by real integration
tests. The initial policy remains narrow and explicit: live delivery is not client
replay, enqueue-time acknowledgement can lose events after a Router crash, and
provider ordering does not promise global ordering.
