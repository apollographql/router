### Fix `@composeDirective` silently dropping composed directives when subgraphs link the same custom spec at different minor versions ([FED-849](https://apollographql.atlassian.net/browse/FED-849))

When two subgraphs linked the same custom spec but at different minor versions (e.g., `v1.0` and `v1.1`), and the higher-version subgraph was passed first in the composition call, the `satisfies` version check inside `ComposeDirectiveManager::validate()` would evaluate `v1.0.satisfies(v1.1) = false` and mark the entire spec as `wont_merge` — silently dropping all composed directives from both subgraphs with no error.

The fix sorts each spec identity's item list by minor version ascending before the inner loop, so the version cursor only ever moves upward and minor-compatible combinations are never incorrectly rejected.

By [@mateusgoettems](https://github.com/mateusgoettems) in https://github.com/apollographql/router/pull/8936
