### Reload OCI artifacts when a tag reference changes ([PR #8805](https://github.com/apollographql/router/pull/8805))

You can now configure tag-based OCI references in the router. When you use a tag reference such as `artifacts.apollographql.com/my-org/my-graph:prod`, the router polls and reloads when that tag points to a new artifact.

This also applies to automatically generated variant tags and custom tags.

By [@graytonio](https://github.com/graytonio) in https://github.com/apollographql/router/pull/8805