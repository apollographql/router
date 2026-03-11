### Add `experimental_hoist_orphan_errors` to control orphan error path assignment

The GraphQL specification requires that errors include a `path` pointing to the most specific field where the error occurred. When a subgraph returns entity errors without valid paths, the router's default behavior is its closest attempt at spec compliance: it assigns each error to every matching entity path in the response. This is the correct behavior when subgraphs respond correctly.

However, when a subgraph returns a large number of entity errors without valid paths — for example, 2000 errors for 2000 expected entities — this causes a multiplicative explosion in the error array that can lead to significant memory pressure and out-of-memory kills. The root cause is the subgraph: a spec-compliant subgraph includes correct paths on its entity errors, and fixing the subgraph is the right long-term solution.

The new `experimental_hoist_orphan_errors` configuration provides an important mitigation while you work toward that fix. When enabled, the router assigns each orphaned error to the nearest non-array ancestor path instead of duplicating it across every entity. This trades spec-precise path assignment for substantially reduced error volume in the response — a conscious trade-off, not a strict improvement.

To target a specific subgraph:

```yaml
experimental_hoist_orphan_errors:
  subgraphs:
    my_subgraph:
      enabled: true
```

To target all subgraphs:

```yaml
experimental_hoist_orphan_errors:
  all:
    enabled: true
```

To target all subgraphs except one:

```yaml
experimental_hoist_orphan_errors:
  all:
    enabled: true
  subgraphs:
    noisy_one:
      enabled: false
```

Per-subgraph settings override `all`. Note that this feature reduces the number of propagated errors but doesn't impose a hard cap — if your subgraph returns an extremely large number of errors, the router still processes all of them.

You'll likely know if you need this. Use it sparingly, and enable it only if you're affected and have been advised to do so. The behavior of this option is expected to change in a future release.

For full configuration reference and additional examples, see the [`experimental_hoist_orphan_errors` documentation](https://www.apollographql.com/docs/graphos/routing/configuration/yaml#experimental_hoist_orphan_errors).

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/8998
