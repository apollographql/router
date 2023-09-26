### Coprocessors: Allow to return with an error message ([PR #3806](https://github.com/apollographql/router/pull/3806))

As mentionned in the [Coprocessors documentation](https://www.apollographql.com/docs/router/customizations/coprocessor#terminating-a-client-request) you can (again) return an error message string in the body of a coprocessor request:

```json
{
    "version": 1,
    "stage": "SubgraphRequest",
    "control": {
        "break": 401
    },
    "body": "my error message"
}
```

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/3806
