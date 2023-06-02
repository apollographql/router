### An APQ query with a mismatched hash will error as HTTP 400 ([Issue #2948](https://github.com/apollographql/router/issues/2948))

We now model the both the behavior of the Gateway and the intended behavior of [the implementation](https://www.apollographql.com/docs/apollo-server/performance/apq/).  Even if our previous behavior was still acceptable, any other behavior is a misconfiguration of a client and should be prevented early.

Previously, if a client sent an operation with an APQ hash, we would merely log an error to the console, **not** register the operation (for the next request) but still execute the query.  We now return a GraphQL error and don't execute the query.  No clients should be impacted by this, though anyone who had hand-crafted a query **with** APQ information (for example, copied a previous APQ-registration query but only changed the operation without re-calculating the SHA-256) might now be forced to use the correct hash (or more practically, remove the hash).

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/3128
