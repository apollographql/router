### Keep every component when normalizing a schema whose definitions and extensions interleave ([PR #10366](https://github.com/apollographql/router/pull/10366))

`normalize_valid_schema` grouped a type's fields and directive applications by origin (definition or extension) using only consecutive runs, so when components of the same origin were not adjacent, a later run replaced an earlier one and its components were dropped. Normalization itself produces that interleaving, so normalizing an already-normalized schema lost fields. Components are now grouped by origin across the whole type. This utility is only used in tests today; no composition or query planning output changes.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10366
