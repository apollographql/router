### Fix Uplink URLs ([Issue #2827](https://github.com/apollographql/router/issues/2827))

The Uplink URLs that come with the router were incorrect. The primary URL worked, but the backup URL did not.

This fixes and adds an integration test to ensure that all Uplink URLs can be contacted and data retrieved.

Users of older versions of the router should upgrade as soon as possible.
However, if this is not possible then a workaround is to set `APOLLO_UPLINK_ENDPOINTS=https://uplink.api.apollographql.com/,https://aws.uplink.api.apollographql.com/`.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/2830
