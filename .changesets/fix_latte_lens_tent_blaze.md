### Internal `apollo_private.*` attributes are not sent to the Jaeger collector ([PR #6033](https://github.com/apollographql/router/pull/6033))

When using Jaeger collector to send traces you will no longer receive span attributes with the `apollo_private.` prefix which is a reserved internal keyword not intended to be externalized.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/6033