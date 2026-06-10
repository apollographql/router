### Add header masking for sensitive data in logs and telemetry ([PR #9155](https://github.com/apollographql/router/pull/9155))

Adds header masking configuration to automatically mask sensitive header values in router logs, telemetry events, and coprocessor communications. This prevents accidental exposure of credentials, API keys, session tokens, and other sensitive information in observability data.

**Key Features:**

- **Automatic masking** of common sensitive headers (authorization, cookie, x-api-key, etc.)
- **Fail-secure by default** — when no `masking:` block is configured, masking is enabled with a built-in sensitive-header list (authorization, cookie, set-cookie, x-api-key, etc.)
- **Independently configurable request and response masking** — different sensitive lists for headers leaving the router vs. headers coming back
- **Global and per-subgraph configuration** — set defaults under `headers.all` and override for specific subgraphs
- **Connector inheritance** — connectors inherit masking rules from their parent subgraph
- **Comprehensive coverage** across `router.request`, `router.response`, `supergraph.request`, `supergraph.response`, `connector.request`, `connector.response` telemetry events, coprocessor logging, OpenTelemetry spans, and Apollo trace-report header forwarding (`telemetry.apollo.send_headers`) — which now redacts the same sensitive headers as the rest of header masking instead of only a hardcoded `authorization`/`cookie`/`set-cookie` set
- **Case-insensitive matching** for header names

**Configuration:**

Masking is configured within the `headers` plugin, nested under `request` and/or `response` sections. By default both global and per-subgraph `sensitive_headers` lists are **additive**: any entries you provide are added to the built-in fail-secure list (authorization, cookie, set-cookie, x-api-key, …). Set `replace_defaults: true` on a global or per-subgraph block to opt out of the built-ins and treat that block's list as authoritative. A subgraph that enables masking always inherits the built-in defaults even when global masking is disabled.

```yaml
headers:
  # Global defaults applied to all subgraphs
  all:
    request:
      masking:
        enabled: true  # default
        # Additional headers to mask on top of the built-in fail-secure list.
        sensitive_headers:
          - x-custom-secret
    response:
      masking:
        enabled: true
        sensitive_headers:
          - x-internal-trace-id

  # Per-subgraph extensions (added to global + built-ins).
  subgraphs:
    products:
      request:
        masking:
          enabled: true
          sensitive_headers:
            - x-products-api-key

  # Example: replace the built-in list entirely (advanced).
  # all:
  #   request:
  #     masking:
  #       replace_defaults: true
  #       sensitive_headers:
  #         - x-only-this
```

**Per-selector override (telemetry):**

Telemetry header selectors — custom span/event/instrument attributes that read a request or response header — accept an optional `redact` field to override the masking rules for that single attribute:

- `redact: mask` — always mask this header's value, regardless of the masking config.
- `redact: allow` — always emit the raw value, ignoring the masking rules.
- omitted (default) — defer to the configured global/per-subgraph masking rules.

```yaml
telemetry:
  instrumentation:
    spans:
      router:
        attributes:
          my.auth.header:
            request_header: authorization
            redact: mask
```

> **Note:** Telemetry emitted at the shared `http_client` transport layer uses the global masking rules, because that layer has no subgraph identity. Per-subgraph overrides still apply at the subgraph and connector telemetry layers, and the global rules include the fail-secure defaults.
>
> **Note:** Masking applies to header *values*. In coprocessor debug logs, only the headers are masked — a request `body` or `context` that a coprocessor copies a sensitive header into is logged verbatim, so avoid placing secrets there if debug logging is enabled.

When enabled, sensitive header values are replaced with `***MASKED***` in debug logs and telemetry output while preserving header names for debugging purposes.

By [@zachfettersmoore](https://github.com/zachfettersmoore) in https://github.com/apollographql/router/pull/9155
