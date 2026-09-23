### Report the resolved license source at startup

Previously, the router's startup logs and metrics only showed whether a graph-artifact reference
was *configured* — not which license source actually won. Since supplying a graph-artifact
reference doesn't guarantee it's used (an explicit license file or `APOLLO_ROUTER_LICENSE` env var
still takes precedence), this made it hard to tell, from logs or telemetry alone, whether a router
was actually licensing off Graph Artifacts (OCI), Uplink, a file, or an environment variable.

The router now logs its resolved license source at startup (`using <source> as license source`)
and reports it as a new `opt.apollo.license.source` attribute on the existing
`apollo.router.config.env` metric, alongside the other CLI/env-derived startup facts already
reported there.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/####
