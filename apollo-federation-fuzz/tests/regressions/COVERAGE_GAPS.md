# Coverage gaps: real planner bugs we'd fail to find today

Mined from `CHANGELOG.md` (router) + `FED-*` tickets in source comments. Each
row is a real planner bug that shipped, with the directive/feature surface
required to reproduce it. The generator currently produces **none** of these
surfaces, which is why the cross-version harness reports 0 plan-algorithm
divergences (everything found so far has been wire-format drift).

| Bug / PR | Surface to reproduce | Generator gap |
|---|---|---|
| **FED-505** ([test](../../apollo-federation/src/correctness/query_plan_analysis_test.rs#L321)) — missing ConditionNode for `@include`/`@skip` causes over-fetching on interface implementations | interface type + implementation; inline fragments with `@include(if: $v)` and `@skip(if: $v)` over the same field; variable boolean | **no interfaces, no `@skip`/`@include`, no operation variables** |
| **PR [#8016](https://github.com/apollographql/router/pull/8016)** — `@requires` subgraph jump fetches `@key` from wrong subgraph | a `@requires` field where the planner has multiple options for sourcing the key, and the chosen upstream doesn't actually have it | **only single-hop `@requires`; no chaining (A requires from B which requires from C)** |
| **PR [#7929](https://github.com/apollographql/router/pull/7929)** — progressive `@override` on interface implementations panics planner: "Was not able to find any options for {}" | interface implemented locally in a subgraph + `@override(label: "...")` on both the interface and its implementing fields + queries traversing the override | **no `@override`, no interfaces** |
| **PR [#8109](https://github.com/apollographql/router/pull/8109)** — `__typename` selection fails on interface object types: "potentially an interface object type at runtime" | `@interfaceObject` + `__typename` query selection | **no `@interfaceObject`** |
| **PR [#7580](https://github.com/apollographql/router/pull/7580)** — invalid `... on Query` spread in deferred fetch when subgraph renames root | `@defer` + subgraph schema with `schema { query: RootQuery }` (renamed root type) | **no `@defer`, no renamed root types** |
| **PR [#9123](https://github.com/apollographql/router/pull/9123)** (reverted in [#9250](https://github.com/apollographql/router/pull/9250)) — defer dependencies dropped after transitive reduction | non-trivial `@defer` plan with a fetch graph that exercises transitive reduction | **no `@defer`** |
| **FED-301** ([test](../../apollo-federation/tests/query_plan/build_query_plan_tests/debug_max_evaluated_plans_configuration.rs)) — query-plan search space blowup | many overlapping providers for a single field path | **possible to hit but unlikely with our small schemas** |
| **FED-251** ([test](../../apollo-federation/tests/query_plan/build_query_plan_tests/introspection_typename_handling.rs)) — `__typename` mishandling | `__typename` selections on abstract / shared root types | **op_gen via apollo-smith may include `__typename`, but we don't actively cover the surface** |
| **FED-515** — type-conditioned fetching ambiguity on merged abstract types | abstract types (interface or union) with inconsistent runtime types across subgraphs | **no abstract types** |
| **Multiple `@key` directives** on one entity ([CHANGELOG](../../CHANGELOG.md)) | `type T @key(fields: "id") @key(fields: "sku") { ... }` and queries that pick different paths | **only single-field single-`@key` per entity** |

## Priority-ordered gaps to close (cheapest → highest payoff)

1. **Operation-level: variables + `@skip`/`@include` on selections.** Apollo-smith already supports this; we just don't ask for it. Lowest effort, instantly opens up the FED-505 surface.
2. **Schema-level: renamed root types.** One-liner in the generator (`schema { query: RootQuery0 }`). Opens PR #7580 surface (combined with `@defer` later).
3. **Schema-level: multiple `@key` directives per entity, including compound keys (`fields: "id sku"`).** Modest effort. Several historical bugs around key-set selection.
4. **Schema-level: inter-entity references + multi-hop `@requires`.** Real schemas have `type T1 { other: T2 }`; this is where most planner divergences live. Requires per-subgraph entity stub coordination.
5. **Schema-level: `@override` (with and without progressive labels).** Moderate effort. PR #7929 territory, recurring bug class.
6. **Schema-level: interfaces + implementation.** Large jump in surface area; opens FED-505, PR #7929, FED-515.
7. **Schema-level: `@interfaceObject`.** Niche but historically buggy (PR #8109).
8. **Schema-level: `@defer` (op-level too).** Active regression in flight (PR #9123 reverted in #9250).
9. **Mutations and subscriptions.** Lower priority; planner paths are similar to queries.

The order above is what I'd implement next. **(1) and (2) together are <100 LOC**
each and together can plausibly find a real divergence against an older
baseline — they directly poke at FED-505 and PR #7580.
