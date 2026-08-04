### Add a native, fail-closed OPA policy provider

Routers can evaluate `@policy` labels with native OPA providers. Configure
`authorization.policy.providers` and routing, keep `authorization.directives.enabled: true`,
and explicitly allowlist claim, header, variable, and context names. Claims are not forwarded by
default. Provider calls use the shared HTTP client, support HTTP(S) and Unix sidecars, bounded
250ms total evaluation by default, two attempts, static transport headers, and temporary replica
ejection. Provider failures reject with HTTP 503 by default; `failure.mode: deny` continues field
filtering with all policies denied. Use `directives.dry_run` to observe failures without rejecting.

Native provider decisions take precedence over Rhai and coprocessor policy context when configured.
