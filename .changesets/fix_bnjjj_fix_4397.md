### Fix response format for statically skipped root selection set ([Issue #4397](https://github.com/apollographql/router/issues/4397))

Previously, the Apollo Router didn't return responses with the same format for some operations with a root selection set that were skipped by `@skip` or `@include` directives.  

For example, if you hardcoded the parameter in a `@skip` directive:

```graphql
{
    get @skip(if: true) {
        id
        name
    }
}
```

Or if you used a variable:

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
