### Fix inconsistency in environment variable parsing for telemetry ([Issue #3203](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Previously, the router would complain when using the rover recommendation of `APOLLO_TELEMETRY_DISABLED=1` environment
variable. Now any non-falsey value can be used, such as 1, yes, on, etc..

By [@nicholascioli](https://github.com/nicholascioli) in https://github.com/apollographql/router/pull/4549
