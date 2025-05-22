### Headers propagated with Router YAML config take precedence over headers from Connector directives ([PR #7499](https://github.com/apollographql/router/pull/7499))

When configuring the same header name in both `@connect(http: { headers: })` (or `@source(http: { headers: })`) in SDL and `propagate` in Router YAML configuration, the request had both headers, even if the value is the same. After this change, Router YAML configuration always wins.

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/7499
