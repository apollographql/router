### Allow coprocessor to return error message ([PR #3806](https://github.com/apollographql/router/pull/3806))

Previously, a regression prevented an error message string from being returned in the body of a coprocessor request. That regression has been fixed, and a coprocessor can once again [return with an error message](https://www.apollographql.com/docs/router/customizations/coprocessor#terminating-a-client-request):

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
