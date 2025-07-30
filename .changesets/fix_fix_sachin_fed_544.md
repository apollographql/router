### Fix query planning error where `@requires` subgraph jump fetches `@key` from the wrong subgraph ([PR #8016](https://github.com/apollographql/router/pull/8016))

During query planning, a subgraph jump added due to a `@requires` field may sometimes try to collect the necessary `@key` fields from an upstream subgraph fetch as an optimization, but it wasn't properly checking whether that subgraph had those fields. This is now fixed, and previously could cause query planning errors with messages that look like "InternalRebaseError(CannotRebase { field_position: Object(T.id), parent_type: Object(T) })".

By [@sachindshinde](https://github.com/sachindshinde) in https://github.com/apollographql/router/pull/8016