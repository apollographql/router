### Prevent subgraph request body being loaded into memory if compression is not enabled ([Issue #4647](https://github.com/apollographql/router/issues/4648))

Previously, even if compression was not enabled, the entire body of the request was loaded into memory.
This PR adds logic to detect either `identity` or a missing content-type, and allows the request through untouched.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4662
