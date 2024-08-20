### Fix session counting and the reporting of file handle shortage ([PR #5834](https://github.com/apollographql/router/pull/5834))

Session counting incorrectly included connections to the health check or other non-graphql connections. This is now corrected so that only connections to the main graphql port are counted.

Warnings about file handle shortages are now handled correctly as a global resource.

The listening logic had its own custom rate limiting notifications. This has been removed and log notification is now controlled by the [standard router log rate limiting configuration](https://www.apollographql.com/docs/router/configuration/telemetry/exporters/logging/stdout/#rate_limit)

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/5834
