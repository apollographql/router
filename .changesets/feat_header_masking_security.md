### Add header masking for sensitive data in logs and telemetry ([Issue #GRAPHOS-85](https://apollographql.atlassian.net/browse/GRAPHOS-85), [Issue #GRAPHOS-86](https://apollographql.atlassian.net/browse/GRAPHOS-86), [PR #TBD](https://github.com/apollographql/router/pull/TBD))

Adds a new global `header_masking` configuration to automatically mask sensitive header values in router logs, telemetry events, and coprocessor communications. This prevents accidental exposure of credentials, API keys, session tokens, and other sensitive information in observability data.

**Key Features:**

- **Automatic masking** of common sensitive headers (authorization, cookie, x-api-key, etc.)
- **Global configuration** with customizable list of headers to mask
- **Fail-secure by default** - masking is enabled by default with sensible defaults
- **Comprehensive coverage** across:
  - Telemetry events (router, supergraph, subgraph, execution, connector)
  - Coprocessor request/response logging
  - OpenTelemetry spans and attributes
- **Case-insensitive matching** for header names
- **Preserves non-sensitive headers** for debugging

**Configuration:**

```yaml
header_masking:
  enabled: true  # default
  sensitive_headers:
    - authorization
    - cookie
    - x-api-key
    - x-custom-secret  # add custom headers
```

When enabled, sensitive header values are replaced with `***MASKED***` in debug logs and telemetry output while preserving header names for debugging purposes.

By [@zachfettersmoore](https://github.com/zachfettersmoore) in https://github.com/apollographql/router/pull/TBD
