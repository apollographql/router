### Supergraph coprocessor implementation ([PR #3647](https://github.com/apollographql/router/pull/3647))

Coprocessors now support supergraph service interception. 

On the request side, the coprocessor payload can contain:
- method
- headers
- body
- context
- sdl

On the response side, the payload can contain:
- status_code
- headers
- body
- context
- sdl

The supergraph request body contains:
* query
* operation name
* variables
* extensions

The supergraph response body contains:
* label
* data
* errors
* extensions

When using `@defer` or subscriptions a supergraph response may contain multiple GraphQL responses, and the coprocessor will be called for each.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3647