### Fix null propagation across multiple fragment spreads via spec-correct CollectFields ([PR #TBD](https://github.com/apollographql/router/pull/TBD))

When a query uses multiple fragment spreads on the same parent type and a subgraph response is missing a required non-null field, the router could return a partial object (e.g. `{"__typename": "A"}`) instead of `null`.  This violates GraphQL spec §6.4.4, which requires that a non-null violation propagates `null` upward to the nearest nullable parent.

The root cause was that the response formatter processed fragment spreads sequentially.  When the first fragment correctly nullified a field, a subsequent fragment on the same parent type would re-process the same field from the original input and overwrite the correctly-propagated `null`.

This fix replaces the sequential fragment loop with a two-phase model matching GraphQL spec §6.3.2:

- *Phase 1 CollectFields*: traverse the entire selection set once, resolving all fragments and merging sub-selections for fields sharing a response key.
- *Phase 2 ExecuteSelectionSet*: process each response key exactly once.

Because each key is visited at most once, no later fragment can overwrite nullification applied by an earlier one.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/TBD
