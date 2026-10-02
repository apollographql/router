### Emit startup warning when deprecated `apollo.preview_entity_cache` plugin is used ([PR #9631](https://github.com/apollographql/router/pull/9631))

The `apollo.preview_entity_cache` plugin is deprecated and will be removed in Router 3.0. A warning is now logged at startup when it is enabled.

Migrate to `apollo.response_cache`, which supersedes it. The two plugins are mutually exclusive and cannot be enabled at the same time.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9631
