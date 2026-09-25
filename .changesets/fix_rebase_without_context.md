### Allow compatible field rebasing on subgraphs without `@fromContext`

Rebasing a field onto a compatible type no longer reports an internal error when the subgraph's federation version predates `@fromContext`. Contextual arguments are still checked on schemas that define the directive.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10287
