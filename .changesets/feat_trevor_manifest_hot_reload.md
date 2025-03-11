### feat: Introduce PQ Manifest `hot_reload` option for local manifests ([PR #6987](https://github.com/apollographql/router/pull/6987))

This change introduces a `persisted_queries.hot_reload` configuration option in order to allow the router to hot reload local PQ manifest changes.

This change explicitly does _not_ piggyback on the existing `--hot-reload` flag.

By [@trevor-scheer](https://github.com/trevor-scheer) in https://github.com/apollographql/router/pull/6987
