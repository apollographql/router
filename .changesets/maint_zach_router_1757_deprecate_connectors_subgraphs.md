### Deprecate `connectors.subgraphs` configuration field ([PR #9415](https://github.com/apollographql/router/pull/9415))

The `connectors.subgraphs` configuration field is now deprecated in favor of `connectors.sources`. When `connectors.subgraphs` is set, the router will emit a deprecation warning at startup directing operators to rename the key. The field will be removed in a future 3.x release.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/9415
