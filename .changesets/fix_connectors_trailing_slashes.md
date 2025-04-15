### Preserve trailing slashes in Connectors URIs

Previously, a URI like `@connect(http: {GET: "/users/"})` could be normalized to `@connect(http: {GET: "/users"})`. This
change preserves the trailing slash, which is significant to some web servers.

## Features

1. Expressions can now be used _anywhere_ in a URI template. Previously, we only allowed expressions in very specific
   locations (a path segment, immediately following a query param name and `=`). Once we have a way to opt out of
   percent encoding, this will allow more dynamic base URLs and constructing complex query params via mapping
   expression.

By [@dylan-apollo](https://github.com/dylan-apollo) in https://github.com/apollographql/router/pull/7220
