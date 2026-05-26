### Document how to log operations that exceed a candidate operation limit ([PR #9294](https://github.com/apollographql/router/pull/9294))

Adds a new section to the request limits docs showing how to use a custom telemetry event with a `gt` condition on the `query` selector to log operations that exceed a candidate `max_aliases`, `max_depth`, `max_height`, or `max_root_fields` value — without configuring `limits` or enabling `warn_only` mode. The example also captures the client name and version from the `apollographql-client-name` and `apollographql-client-version` headers so you can see which clients are sending the offending operations. The `warn_only` section now cross-references this approach.

By [@smyrick](https://github.com/smyrick) in https://github.com/apollographql/router/pull/9294
