### Fix session counting and the reporting of file handle shortage ([PR #5834](https://github.com/apollographql/router/pull/5834))

The router previously gave incorrect warnings about file handle shortages due to session counting incorrectly including connections to health-check connections or other non-GraphQL connections. This is now corrected so that only connections to the main GraphQL port are counted, and file handle shortages are now handled correctly as a global resource.

Also, the router's port listening logic had its own custom rate-limiting of log notifications. This has been removed and replaced by the [standard router log rate limiting configuration](https://www.apollographql.com/docs/router/configuration/telemetry/exporters/logging/stdout/#rate_limit)

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/5834
