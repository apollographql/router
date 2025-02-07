### Remove `experimental_` prefix for PQ `local_manifests` configuration

The `experimental_local_manifests` PQ configuration option is being promoted to stable. This change updates the configuration option name and any references to it, as well as the related documentation. The `experimental_` usage remains valid as an alias for existing usages.

By [@trevor-scheer](https://github.com/trevor-scheer) in https://github.com/apollographql/router/pull/6564