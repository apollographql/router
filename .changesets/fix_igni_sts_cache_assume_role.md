### Implement manual caching for AWS Security Token Service credentials ([PR #5508](https://github.com/apollographql/router/pull/5508))

In the AWS Security Token Service (STS), the `CredentialsProvider` chain includes caching, but this functionality was missing for `AssumeRoleProvider`.
This change introduces a custom `CredentialsProvider` that functions as a caching layer with these rules:

- **Cache Expiry**: Credentials retrieved are stored in the cache based on their `credentials.expiry()` time if specified, or indefinitely (`ever`) if not.
- **Automatic Refresh**: Five minutes before cached credentials expire, an attempt is made to fetch updated credentials.
- **Retry Mechanism**: If credential retrieval fails, another attempt is scheduled after a one-minute interval.
- (Coming soon, not included in this change) **Manual Refresh**: The `CredentialsProvider` will expose a `refresh_credentials()` function. This can be manually invoked, for instance, upon receiving a `401` error during a subgraph call.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/5508
