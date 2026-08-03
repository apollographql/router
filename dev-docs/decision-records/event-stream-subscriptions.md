# Provider-neutral event streams for federated subscriptions

## Status

Implemented

## Context

Federated subscriptions normally obtain their primary event from a subgraph over
WebSocket or callback, then execute the rest of the query plan for every event.
Some graphs instead publish entity-shaped events to NATS, Kafka, Redis Pub/Sub,
or another event system.

The Router needs to consume those events without coupling composed graph
metadata, Router configuration, or federated execution to a particular provider.
The public configuration also needs room to grow because moving or renaming its
fields later would be a breaking change.

## Decision

### Keep configuration provider-neutral

Event configuration has three stable namespaces. Its initial version is implicit;
there is deliberately no `version` field.

```yaml
events:
  providers:
    production-events:
      type: nats_core
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
- `sources` gives composed metadata a provider-independent logical name.
- `policies` owns reusable, externally observable delivery semantics.

Provider connection settings live in `providers.*.config`. Provider consumer
settings live in `sources.*.provider_options`, and format settings live in
`sources.*.format.config`. Each extension point is an object validated by its
selected implementation.

Common behavior uses tagged objects (`type: ...`) instead of booleans. Future
behavior can therefore be introduced by adding a variant or optional field
without moving existing values or changing a scalar into an object.

### Keep composed metadata logical

The event linked specification identifies a logical source and
provider-neutral destinations:

```graphql
type Subscription {
  productUpdated: Product!
    @event__subscribe(
      source: "product-updates"
      destinations: ["products.updated"]
    )
}
```

Destinations may interpolate scalar arguments with `{{ args.name }}`, including
nested input paths such as `tenant.{{ args.scope.id }}`. Resolved destinations
are part of trigger identity. Authorization therefore runs before event setup,
and destination templating is not a substitute for field authorization.

Composed metadata must not contain broker URLs, credentials, provider types,
consumer groups, or delivery policies. Router configuration maps each logical
source to those physical details.

### Integrate through a Tower service boundary

An `EventSubscriptionLayer` wraps the normal subscription fetch service. It
recognizes event-backed subscription nodes and delegates every other request to
the existing path. The event service owns subscription admission, tracing, error
mapping, and installation into the existing subscription task. The event runtime
owns catalog lookup, destination rendering, provider selection, decoding, and
local trigger sharing.

```text
subscription fetch request
          |
          v
EventSubscriptionLayer -- non-event --> existing subscription service
          |
        event
          v
logical source -> provider adapter -> shared bounded fan-out
                                      -> graphql_entity decoder
                                      -> existing federated hydration
                                      -> client stream
```

This boundary keeps broker behavior out of federated execution and lets event
subscriptions reuse the Router's established subscription lifecycle. Provider
adapters are deliberately ordinary runtime components rather than separate Tower
services.

Equivalent triggers share one provider subscription inside a Router process.
Trigger identity includes every input affecting event selection: provider,
logical source, and resolved destinations. Local fan-out is bounded by the
selected policy, and a lagging subscriber observes `drop_oldest` behavior.

### Map the common policy onto each provider

The implementation supports four provider identifiers:

| Provider | Native subscription behavior |
| --- | --- |
| `nats_core` | Uses ordinary subscriptions, never queue groups, so every Router instance receives each matching live event. |
| `nats_jetstream` | Creates an ephemeral pull consumer at the live edge with explicit acknowledgement after local enqueue. |
| `redis_pubsub` | Supports channel, pattern, and sharded subscriptions with reconnect handling; delivery remains at-most-once. |
| `kafka` | Uses an instance- and trigger-unique consumer group, starts at latest, disables automatic commits, and commits after local enqueue. |

Provider and source objects are validated when a Router pipeline is built, so an
unsupported provider type or misspelled provider option fails the reload rather
than the first subscription operation.

`every_router_instance` means every live Router process receives every matching
event. Each adapter must preserve that topology even when the provider's default
consumer-group behavior would load-balance messages.

## Compatibility

The following public shapes are stable:

- The `events.providers`, `events.sources`, and `events.policies` separation.
- Logical source names and provider-neutral destinations in composed metadata.
- `type` discriminators on providers, formats, and policy variants.
- Object-valued `config` and `provider_options` extension points.
- Explicit delivery, start, acknowledgement, buffering, distribution, and
  ordering semantics.

New providers, tagged policy variants, and optional fields may be added. Existing
variants retain their meaning. A design that cannot be expressed by those
additions requires an explicit migration rather than a silent semantic change.
A top-level feature version should be introduced only if a second incompatible
configuration grammar is needed.

Internal provider contracts, trigger keys, health state, and subscription
envelopes are not part of this compatibility promise.

## Consequences

The same graph metadata and subscription execution work with every implemented
provider. Operators can replace a provider by changing Router configuration
without recomposing the graph, while each adapter retains the native state needed
to meet the shared policy.

The initial policy intentionally remains narrow: delivery is live rather than
client-replayable, acknowledgement after local enqueue can lose an event if a
Router crashes, and provider ordering does not imply global ordering.

Operational hardening remains follow-up work: broaden reconnect, redelivery,
slow-client, reload, and shutdown conformance tests; integrate provider state with
health reporting; and add metrics for lag, acknowledgement, decode failures, and
hydration. Replay, durable client cursors, and alternative distribution semantics
must be introduced as new tagged policy variants rather than changing the current
ones.
