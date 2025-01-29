### Demand control lookup optimizations ([PR #6450](https://github.com/apollographql/router/pull/6450))

Demand Control can reduce router throughput due to the extra processing required for scoring. This change shifts more data to be computed at plugin initialization and consolidates lookup queries.

- Cost directives for arguments are now stored in a map alongside those for field definitions
- All precomputed directives are bundled into a struct for each field, along with that field's extended schema type. This reduces 5 individual lookups to a single lookup.
- Response scoring was looking up each field's definition twice. This is now reduced to a single lookup.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/6450
