# Router WebAssembly extension contract

This directory contains the component-model contract between Apollo Router and WebAssembly extensions. The first implementation supports `supergraph.request`, `subgraph.request`, and `connector.request` hooks. Extensions receive only explicitly permitted request data and return declarative mutations or an early response; they never receive direct access to Router internals.

## Request identity

Every event includes its hook name and Router request ID. Optional identity and transport fields are populated at the boundaries where they apply:

| Hook | `service-name` | `source-name` | `connector-name` | `transport` | HTTP fields |
| --- | --- | --- | --- | --- | --- |
| `supergraph.request` | absent | absent | absent | absent | populated |
| `subgraph.request` | originating subgraph | absent | absent | absent | populated |
| `connector.request` | originating subgraph | connector source, when named | connector ID or schema coordinate | `http` or `mapping_only` | populated for HTTP; absent for mapping-only |

For connector events, `body` is the raw transport request body rather than a GraphQL request. The mutation's optional `method`, `uri`, and `body` fields can update an HTTP connector transport request when the hook has write permission for that field. A mapping-only connector has no HTTP request, so event HTTP fields are absent or empty and transport mutations are rejected. The separate optional identity fields avoid encoding multiple identities into a composite string and leave room for additional transports without changing the event record.

## Compatibility strategy

The Router YAML intentionally has no version field. Compatibility is divided into two independently evolving contracts:

- The Router configuration schema uses tagged source variants, named hooks, opaque plugin-owned `configuration`, selectors, permissions, limits, and failure behavior. New optional fields and new tagged variants can be added without changing existing configurations.
- The component contract is versioned by its WIT package (`apollo:router-plugin@0.1.0`). A future incompatible ABI is introduced as a new WIT package version, and the host can support old and new component worlds concurrently. Users do not select an ABI version in YAML; the component declares what it imports and exports.

The source digest is colocated with the source because it verifies the bytes resolved by that source. There is no separate verification policy. Digest verification is opt-in by presence now; future source variants can add source-specific signature, identity, or trust metadata without adding unrelated top-level policy.

## Security boundary

The host starts each invocation with no inherited environment, stdio, filesystem, TCP, or UDP capabilities. Header, context, GraphQL request, and connector transport request access is allowlisted per hook and defaults to none. The host validates every returned mutation rather than trusting the guest to honor its declared permissions.

Each plugin has bounded execution time, queue wait, linear memory, concurrency, queue depth, and input/output size. Components are compiled at Router initialization, while stores and component instances are isolated per invocation.

## Expansion plan

1. Stabilize request hooks and mutation semantics, including connector transport behavior, using conformance fixtures shared by guest SDKs.
2. Add response and additional pipeline hooks as new `hook` names, keeping hook-specific selectors and permissions optional.
3. Add OCI and HTTPS source variants under the existing tagged `source` object, with source-specific immutable references and verification metadata.
4. Add host capabilities as explicit WIT imports and matching YAML permissions; do not inherit ambient WASI access.
5. Support a newer WIT package alongside `0.1.0` before removing an old ABI, with startup diagnostics for unsupported component worlds.
6. Add pooling, lifecycle hooks, metrics, and tracing without changing extension configuration. Compiled-component caching is already an internal host concern and does not affect extension configuration.

These rules reserve the public shape needed for expansion while keeping the first implementation small enough to validate with real guest toolchains.
