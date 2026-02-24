### Validate `ObjectValue` variable fields against input type definitions ([PR #8821](https://github.com/apollographql/router/pull/8821) and [PR #8884](https://github.com/apollographql/router/pull/8884))

The router now validates individual fields of input object variables against their type definitions. Previously, variable validation checked that the variable itself was present but didn't validate the fields within the object.

Example:
```graphql
## schema ##
input MessageInput {
    content: String
    author: String
}
type Receipt {
    id: ID!
}
type Query{
    send(message: MessageInput): Receipt
}

## query ##
query($msg: MessageInput) {
    send(message: $msg) {
        id
    }
}

## input variables ##
{"msg":
    {
    "content": "Hello",
    "author": "Me",
    "unknownField": "unknown",
    }
}
```
This request previously passed validation because the variable `msg` was present in the input, but the fields of `msg` weren't validated against the `MessageInput` type.

> [!WARNING]
> To opt out of this behavior, set the `supergraph.strict_variable_validation` config option to `measure`.

Enabled:
```yaml
supergraph:
  strict_variable_validation: enforce
```

Disabled:
```yaml
supergraph:
  strict_variable_validation: measure
```

By [@conwuegb](https://github.com/conwuegb) in https://github.com/apollographql/router/pull/8821 and https://github.com/apollographql/router/pull/8884
