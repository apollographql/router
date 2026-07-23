### Fix excess CPU and latency from extended metrics reference mode on fragment-heavy operations

`extract_enums_from_selection_set` (used when `telemetry.apollo.metrics_reference_mode` is `extended`, the default) deduplicated fragment spreads with a `HashSet` keyed on `&Object`. Because `&T` hashes and compares by value, every fragment spread deep-hashed the entire response subtree, so extraction cost grew with response size and fragment count. Operations with many nested fragments over large responses saw significant added CPU and tail latency.

The set now keys on the response object's pointer identity, which is `O(1)` and sufficient for the deduplication and cycle protection this function needs. Reported reference data is unchanged.

By [@ebylund](https://github.com/ebylund) in https://github.com/apollographql/router/pull/9840
