### Fix a panic building connector request URIs with an empty or slash-less relative path ([PR #9790](https://github.com/apollographql/router/pull/9790))

Connector request URIs with an empty path and query (no `sourcePath`, `connectPath`, or query params) or with a `connectPath` template lacking a leading `/` could panic the router when built against `http` crate versions 1.4.2 and later, which tightened `PathAndQuery` validation to reject an empty string and require a leading `/` on relative paths.

Connector URI construction now explicitly normalizes these cases (empty becomes `/`, a slash-less relative path gets a leading `/` added) instead of relying on validation behavior that newer `http` versions no longer provide.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9790
