### Relax percent encoding for Connectors ([PR #7220](https://github.com/apollographql/router/pull/7220))

Characters outside of `{ }` expressions will no longer be percent encoded unless they are completely invalid for a
URI. For example, in an expression like `@connect(http: {GET: "/products?filters[category]={$args.category}"})` the
square
braces `[ ]` will no longer be percent encoded. Any string from within a dynamic `{ }` will still be percent encoded.

By [@dylan-apollo](https://github.com/dylan-apollo) in https://github.com/apollographql/router/pull/7220
