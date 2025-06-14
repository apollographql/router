### Improved code organization for Automatic Persisted Queries (APQ) functionality ([PR #7689](https://github.com/apollographql/router/pull/7689))

We've reorganized how APQ (Automatic Persisted Queries) code is structured within the router to make it easier to maintain and extend. The APQ logic that was previously embedded within the subgraph service has been extracted into dedicated, focused modules under a new `services/layers/apq` directory.

This change improves code organization and makes APQ functionality easier to find and work with, while maintaining all existing behavior and compatibility.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/7689
