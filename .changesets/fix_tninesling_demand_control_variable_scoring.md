### Fix demand control panic for custom scalars that represent non-GraphQL-compliant JSON ([PR #6288](https://github.com/apollographql/router/pull/6288))

This panic could be triggered with the following schema:

```
scalar ArbitraryJson

type MyInput {
    json: ArbitraryJson
}

type Query {
    fetch(args: MyInput): Int
}
```

Then, submitting the query

```
query FetchData($myJsonValue: ArbitraryJson) {
    fetch(args: {
        json: $myJsonValue
    })
}
```

and variables

```
{
    "myJsonValue": {
        "field.with.dots": 1
    }
}
```

During scoring, the demand control plugin would attempt to convert the variable structure into a GraphQL-compliant structure requiring valid GraphQL names as keys, but the dot characters in the keys would cause a panic. With this fix, only the GraphQL compliant part of the input object is scored, and the arbitrary JSON marked by the custom scalar is scored as an opaque scalar, similar to how we process built-ins like `Int` or `String`.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/6288
