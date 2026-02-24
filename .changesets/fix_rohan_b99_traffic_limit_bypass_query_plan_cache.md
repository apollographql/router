### Enforce and log operation limits for cached query plans ([PR #8810](https://github.com/apollographql/router/pull/8810))

The router now logs the operation-limits warning for cached query plans as well, ensuring the query text is included whenever limits are exceeded. This also fixes a case where a cached plan could bypass enforcement after changing `warn_only` from `true` to `false` during a hot reload.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8810
