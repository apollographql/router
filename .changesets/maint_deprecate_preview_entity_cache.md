### Emit startup warning when deprecated `apollo.preview_entity_cache` plugin is used ([Issue #1771](https://github.com/apollographql/router/issues/1771))

The `apollo.preview_entity_cache` plugin is deprecated and will be removed in Router 3.x. A warning is now logged at startup when it is enabled.

Migrate to `apollo.response_cache`, which supersedes it. The two plugins are mutually exclusive and cannot be enabled at the same time.
