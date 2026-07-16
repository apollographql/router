### Fix a performance regression in extended reference reporting ([PR #9473](https://github.com/apollographql/router/pull/9473))

With `telemetry.apollo.metrics_reference_mode: extended`, fragment-spread deduplication in `extract_enums_from_selection_set` keyed a `HashSet` on `&Object`, which hashes the whole response subtree on every spread. Extraction cost grew with response size and fragment count. The set now keys on the object's pointer identity.

By [@sethrperkins](https://github.com/sethrperkins) in https://github.com/apollographql/router/pull/9820
