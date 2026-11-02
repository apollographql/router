### Stop copying the Apollo telemetry configuration on every request

The router copied its whole `telemetry.apollo` configuration once per request and once per subgraph or connector call, then discarded the copy. It now reads only the two settings it needs at that point, which lowers CPU use per request. No configuration change is needed.

By [@BrynCooke](https://github.com/BrynCooke)
