### Check if jwt claims is part of the context before getting the jwt expiration with subscriptions ([PR #7069](https://github.com/apollographql/router/pull/7069))

In https://github.com/apollographql/router/pull/6930 we introduced [logs](https://github.com/apollographql/router/pull/6930/files#diff-7597092ab9d509e0ffcb328691f1dded20f69d849f142628095f0455aa49880cR648) in `jwt_expires_in` function which causes a lot of logs when using subscriptions. 
It also unveils a bug in the subscription implementation with JWT. Indeed if there was not JWT claims in the context, before we set a timeout set at `Duration::MAX`. Now it's always pending and there's no timeout anymore.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7069