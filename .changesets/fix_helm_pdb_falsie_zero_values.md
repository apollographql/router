### Fix Helm PDB template ignoring zero values for `minAvailable` and `maxUnavailable` ([Issue #8350](https://github.com/apollographql/router/issues/8350))

Setting `minAvailable: 0` or `maxUnavailable: 0` in the `podDisruptionBudget` Helm values was silently ignored, producing a PDB with no disruption rules. This happened because Go templates treat `0` as falsy in a simple `if` check.

The template now uses `kindIs "invalid"` to correctly detect whether a value was explicitly set, and an `else if` to prevent both fields from being rendered simultaneously — which Kubernetes rejects. If both `maxUnavailable` and `minAvailable` are set, `maxUnavailable` takes precedence.

By [@apollo-mateuswgoettems](https://github.com/apollo-mateuswgoettems) in https://github.com/apollographql/router/pull/9028