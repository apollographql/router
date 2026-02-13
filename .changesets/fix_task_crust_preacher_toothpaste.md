### Fix Router's validation of `ObjectValue` variables ([PR #8821](https://github.com/apollographql/router/pull/8821) and [PR #8884](https://github.com/apollographql/router/pull/8884))

This change addresses an issue in Router whereby invalid additional fields of an input object were able to pass variable validation because the fields of the object were not being properly checked.

Example:
```
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
This request would pass validation because the variable `msg` from the query was present in the input, however, the fields of `msg` from the input were not being validated against the `MessageInput` type.

> [!WARNING]
> If you need to opt out, you must set the `supergraph.strict_variable_validation` config option to `measure` instead.

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
