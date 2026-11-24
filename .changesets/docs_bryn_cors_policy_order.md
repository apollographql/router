### Document how the router chooses between CORS policies ([PR #10890](https://github.com/apollographql/router/pull/10890))

The CORS docs now say that the router uses the first policy in `cors.policies` that matches a request's origin. They used to suggest that an exact `origins` entry took priority over every policy's `match_origins`.

If a broad `match_origins` policy comes before a narrower `origins` policy, the broad policy's settings, such as `allow_credentials`, apply to origins that both policies match. Move the narrower policy first.

The docs also note that a policy without `origins` matches GraphOS Studio by default.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10890
