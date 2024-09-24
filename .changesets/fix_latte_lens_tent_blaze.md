### Don't send `apollo_private.*` attributes to  Jaeger connector ([PR #6033](https://github.com/apollographql/router/pull/6033))


When using Jaeger collector to send traces you will no longer receive useless span attributes prefixed with `apollo_private.`

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/6033