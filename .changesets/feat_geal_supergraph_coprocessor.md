### Supergraph coprocessor implementation ([PR #3408](https://github.com/apollographql/router/pull/3408))

This adds support for coprocessors at the supergraph service level. Supergraph plugins work on the request side with a parsed GraphQL request object, so the query and operation name, variables and extensions are directly accessible. On the response side, they handle GraphQL response objects, with label, data, path, errors, extensions. The supergraph response contains a stream of GraphQL responses, which can contain multiple elements if the query uses `@defer` or subscriptions. When configured to observe the responses, the coprocessor will be called for each of the deferred responses.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3408