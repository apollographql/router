### Prevent Datadog timeout errors in logs ([Issue #2058](https://github.com/apollographql/router/issue/2058))

The telemetry Datadog exporter uses connection pooling by default. However, these pools connections are frequently closed leading to errors in the router logs.

For example:
```
2024-07-19T15:28:22.970360Z ERROR  OpenTelemetry trace error occurred: error sending request for url (http://127.0.0.1:8126/v0.5/traces): connection error: Connection reset by peer (os error 54)
```

The pool timeout for the Datadog exporter is now set to 1 millisecond, which will greatly reduce the frequency that this occurs.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5692
