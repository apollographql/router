### AWS Security Token Service: Implement a manual cache on top of assume_role and credentials provider ([PR #5508](https://github.com/apollographql/router/pull/5508))

While the STS `CredentialsProvider` chain has a cache, it is not the case for `AssumeRoleProvider`.

This changeset introduces a custom CredentialsProvider that operates as a caching layer, with a couple of business rules related to it:

1. When credentials are retrieved, they will be kept in cache for:
     - `credentials.expiry()` if it is set
     - `15 minutes` if not
2. 5 minutes before credentials get removed from cache, we will try to retrieve new ones.
3. Failure to retrieve credentials will trigger a new attempt after `1 minute`
4. `CredentialsProvider` exposes a `refresh_credentials()` function that could be used to manually trigger a refresh, say if the subgraph call yields a `401` (TODO as a followup, not part of this changeset)

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/5508
