### Add header masking for sensitive data in logs and telemetry ([Issue #GRAPHOS-85](https://apollographql.atlassian.net/browse/GRAPHOS-85), [Issue #GRAPHOS-86](https://apollographql.atlassian.net/browse/GRAPHOS-86), [PR #TBD](https://github.com/apollographql/router/pull/TBD))

Adds header masking configuration to automatically mask sensitive header values in router logs, telemetry events, and coprocessor communications. This prevents accidental exposure of credentials, API keys, session tokens, and other sensitive information in observability data.

**Key Features:**

- **Automatic masking** of common sensitive headers (authorization, cookie, x-api-key, etc.)
- **Fail-secure by default** — masking is enabled by default with sensible defaults
- **Global and per-subgraph configuration** — set defaults under `headers.all` and override for specific subgraphs
- **Request and response masking** — configure masking independently for inbound and outbound headers
- **Connector inheritance** — connectors inherit masking rules from their parent subgraph
- **Comprehensive coverage** across telemetry events, coprocessor logging, and OpenTelemetry spans
- **Case-insensitive matching** for header names

**Configuration:**

Masking is configured within the `headers` plugin, nested under `request` or `response` sections:

```yaml
headers:
  # Global defaults applied to all subgraphs
  all:
    request:
      masking:
        enabled: true  # default
        sensitive_headers:
          - authorization
          - cookie
          - x-api-key
          - x-custom-secret  # add custom headers
    response:
      masking:
        enabled: true

  # Per-subgraph overrides
  subgraphs:
    products:
      request:
        masking:
          enabled: true
          sensitive_headers:
            - authorization
            - x-products-api-key
```

When enabled, sensitive header values are replaced with `***MASKED***` in debug logs and telemetry output while preserving header names for debugging purposes.

By [@zachfettersmoore](https://github.com/zachfettersmoore) in https://github.com/apollographql/router/pull/TBD
