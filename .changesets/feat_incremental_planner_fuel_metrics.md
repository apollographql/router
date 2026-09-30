### Add fuel histograms for the incremental query planner

When the incremental query planner is enabled, the router now records two histograms per planned operation:

- `apollo.router.query_planning.plan.fuel_consumed`: fuel the planner spent improving on its first complete plan.
- `apollo.router.query_planning.plan.fuel_remaining`: fuel left when the planner stopped searching. A value of `0` means the planner ran out of fuel before it finished exploring alternatives.

Use them to tune `supergraph.query_planning.incremental_planner.fuel`. Both use the buckets `0`, `1`, `10`, `100`, `1000`, `10000`, `100000`, and `1000000` instead of the global `buckets` setting, since fuel values aren't durations. A view for either metric overrides them. For mutations, which are planned one top-level field at a time, both values come from the field whose search consumed the most fuel.

By [@tninesling](https://github.com/tninesling)
