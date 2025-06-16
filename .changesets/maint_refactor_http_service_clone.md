### HTTP client service now allows backpressure ([PR #7694](https://github.com/apollographql/router/pull/7694))

This internal refactoring caches http_client services rather than recreating them on every call. 
This has no impact on user functionality but is retained in the changelog so that other teams
can be alerted to this change in behavior.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/7694
