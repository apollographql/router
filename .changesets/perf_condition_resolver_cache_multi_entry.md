### Cache condition resolutions across all context/exclusion combinations ([PR #9741](https://github.com/apollographql/router/pull/9741))

Extends the condition resolver cache introduced in #9740 to cache
resolutions for all combinations of `@include`/`@skip` context and
excluded conditions, not just the first combination seen per edge.

Previously, the cache bailed out and skipped caching entirely when the
`OpGraphPathContext` (active `@include`/`@skip` directives) or
`ExcludedConditions` were non-empty. This meant the cache only helped
for the first key of every type and missed opportunities on schemas
where types have multiple keys or `@requires` conditions that are
evaluated under different include/skip contexts.

Now the cache stores a `Vec<CachedConditionEntry>` per edge, where each
entry records the full `(context, excluded_destinations,
excluded_conditions)` triple. On lookup, entries are scanned for an
exact match. On miss (no matching entry found), the new resolution is
inserted — allowing the cache to accumulate results across all
combinations encountered during planning.

On a 14 MB production supergraph with complex `@key`/`@requires`
conditions, this reduces query planning time by 2-9x compared to the
single-entry cache, with under 10% additional memory overhead.

By [@tninesling](https://github.com/tninesling)
