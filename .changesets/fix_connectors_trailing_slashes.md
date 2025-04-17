### Preserve trailing slashes in Connectors URIs ([PR #7220](https://github.com/apollographql/router/pull/7220))

Previously, a URI like `@connect(http: {GET: "/users/"})` could be normalized to `@connect(http: {GET: "/users"})`. This
change preserves the trailing slash, which is significant to some web servers.

By [@dylan-apollo](https://github.com/dylan-apollo) in https://github.com/apollographql/router/pull/7220
