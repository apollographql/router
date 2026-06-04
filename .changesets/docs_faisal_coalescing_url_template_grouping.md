### Document `$()` grouping requirement for `??` and `?!` operators in URL template fields ([PR #9578](https://github.com/apollographql/router/pull/9578))

The null-coalescing (`??`) and none-coalescing (`?!`) operators require `$(...)` grouping syntax when used inside URL template fields (`GET`, `POST`, `path`, `queryParams`). Using them as bare infix expressions produces a confusing `INVALID_URL_PROPERTY: nom::error::ErrorKind::Eof` error because the path parser terminates on `??`/`?!` tokens before they can be consumed as infix operators.

The fix is to wrap the full expression in `$(...)`:

```graphql
# Invalid — produces INVALID_URL_PROPERTY parse error
@connect(http: { GET: "/offers/{$args.workflow ?? \"default\"}" }, ...)

# Valid
@connect(http: { GET: "/offers/{$($args.workflow ?? \"default\")}" }, ...)
```

This requirement applies to all three URL fields: HTTP method fields (`GET`, `POST`, etc.), `path`, and `queryParams`.

By [@faisalwaseem](https://github.com/faisalwaseem) in https://github.com/apollographql/router/pull/9578
