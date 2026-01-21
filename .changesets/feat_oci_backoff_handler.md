### Hot Reload OCI Artifacts

We now allow tag based OCI references to be configured in the router. When using a tag reference such as `artifacts.apollographql.com/my-org/my-graph:prod` router will automatically poll and reload when the artifact referenced by this tag changes. This applies for the automatically generated variant tags as well as any custom tags that may be created.

By [@graytonio](https://github.com/graytonio) in https://github.com/apollographql/router/pull/8805