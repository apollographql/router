### Skip GraphOS tests when Apollo key not present ([PR #5362](https://github.com/apollographql/router/pull/5362))

Some tests require `APOLLO_KEY` and `APOLLO_GRAPH_REF` to execute successfully.
These are now skipped if these env variables are not present.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5362
