## Cooperative Cancellation for Query Planning

This release introduces cooperative cancellation support for query planning operations. This feature allows the router
to gracefully handle timeouts and cancellations during query planning, improving resource utilization and user experience.

### Key Features

- **Query Planning Cancellation**: The router can now optionally cancel long-running query planning operations when a client disconnects or when timeouts occur
- **Enhanced Metrics**: New metrics track query planning outcomes including timeouts and cancellations
- **Configurable Timeouts**: Added configuration options to control cancellation behavior:
  - `experimental_cooperative_cancellation.enforce`: Enable strict cancellation enforcement
  - `experimental_cooperative_cancellation.measure`: Enable cancellation metrics without enforcement
  - Optional timeout configuration for both modes

### Configuration Examples

#### Configuring Enforce Mode

```yaml
supergraph:
  query_planning:
    experimental_cooperative_cancellation:
      enforce: enabled
```

#### Configuring a timeout in Enforce Mode

```yaml
supergraph:
  query_planning:
    experimental_cooperative_cancellation:
      enforce:
        enabled_with_timeout_in_seconds: 1.0
```

#### Configuring Measure Mode

This is the default mode.

```yaml
supergraph:
  query_planning:
    experimental_cooperative_cancellation:
      measure: enabled
```

#### Configuring a timeout in Measure Mode

```yaml
supergraph:
  query_planning:
    experimental_cooperative_cancellation:
      measure:
        enabled_with_timeout_in_seconds: 1.0
```

#### Disabling Cooperative Cancellation

```yaml
supergraph:
  query_planning:
    experimental_cooperative_cancellation: disabled
```

By [@Velfi](https://github.com/Velfi) in https://github.com/apollographql/router/pull/7604
