### Preserve local selections when combining planner path trees

Keep fully local selections when merging or extending path trees, including trees with no children. The equality shortcut now distinguishes different local selections. Extension retains local payloads while preserving the existing child order used for serial mutation planning.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10289
