### Apply type-conditioned flatten rewrites to matching objects inside nested lists ([PR #10369](https://github.com/apollographql/router/pull/10369))

The router's read-only path traversal matches objects of the requested type inside a list nested one level within a type-conditioned flatten step (for example `items.@|[Book]`), but its mutable traversal, which applies data rewrites, skipped those objects entirely. Rewrites now reach the same objects the read path selects.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10369
