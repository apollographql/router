### Fix format_response for statically skipped root selection set  ([Issue #4397](https://github.com/apollographql/router/issues/4397))

Either you have query like this:

```graphql
{
    get @skip(if: true) {
        id
        name
    }
}
```

or 

```graphql
{
    get($skip: Boolean = true) @skip(if: $skip) {
        id
        name
    }
}
```

you'll receive the same response

```json
{ "data": {}}
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4466
