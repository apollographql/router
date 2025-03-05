### Ensure query planning exits when its request times out ([PR #6840](https://github.com/apollographql/router/pull/6840))

When a request times out/is cancelled, resources allocated to that request should be released. Previously, query planning could potentially keep running after the request is cancelled. After this fix, query planning now exits shortly after the request is cancelled. If you need query planning to run longer, you can [increase the request timeout](https://www.apollographql.com/docs/graphos/routing/performance/traffic-shaping#timeouts).

By [@SimonSapin](https://github.com/SimonSapin) and [@sachindshinde](https://github.com/sachindshinde) in https://github.com/apollographql/router/pull/6840