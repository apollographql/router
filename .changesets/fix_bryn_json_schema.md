### Reduce JSON schema size and Router memory footprint ([PR #5061](https://github.com/apollographql/router/pull/5061))

As we add more features to the Router the size of the JSON schema for the router configuration file continutes to grow.  In particular, adding [conditionals to telemetry](https://github.com/apollographql/router/pull/4987) in v1.46.0 significantly increased this size of the schema. This has a noticeable impact on initial memory footprint, although it does not impact service of requests.

The JSON schema for the router configuration file has been optimized from approximately 100k lines down to just over 7k.

This reduces the startup time of the Router and a smaller schema is more friendly for code editors.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5061
