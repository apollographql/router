### Prevent Datadog timeout errors in logs ([Issue #2058](https://github.com/apollographql/router/issue/2058))

The router's Datadog exporter has been updated to reduce the frequency of logged errors related to connection pools.

Previously, the connection pools used by the Datadog exporter frequently timed out, and each timeout logged an error like the following:

```
2024-07-19T15:28:22.970360Z ERROR  OpenTelemetry trace error occurred: error sending request for url (http://127.0.0.1:8126/v0.5/traces): connection error: Connection reset by peer (os error 54)
```

Now, the pool timeout for the Datadog exporter has been changed so that timeout errors happen much less frequently.  

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5692
