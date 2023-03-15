### Retain existing Apollo Uplink entitlements ([PR #2781](https://github.com/apollographql/router/pull/2781))

Our end-to-end integration testing revealed a newly-introduced bug in v1.12.0 which could affect requests to Apollo Uplink endpoints which are located in different data centers, when those results yield differing responses.  This only impacted a very small number of cases, but retaining previous fetched values is undeniably more durable and will fix this so we're expediting a fix.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/2781
