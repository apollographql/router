### Fix format_response for statically skipped root selection set ([Issue #4397](https://github.com/apollographql/router/issues/4397))

If in your GraphQL operation you have a root selection set skipped by `@skip` or `@include` directive, before this fix the results you got if you hardcoded the value of the parameter in `@skip` directive like this for example:

```graphql
{
    get @skip(if: true) {
        id
        name
    }
}
```

or if you used a variable like this:

```graphql
{
    get($skip: Boolean = true) @skip(if: $skip) {
        id
        name
    }
}
```

The router returned responses with different formats.

This release fixes the issue, and the router returns the same response for both examples:

```json
{ "data": {}}
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4466
