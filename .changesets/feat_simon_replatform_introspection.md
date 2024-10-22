### Remove legacy schema introspection ([PR #6139](https://github.com/apollographql/router/pull/6139))

Schema introspection in the router now runs natively without JavaScript. We have high confidence that the new native implementation returns responses that match the previous Javascript implementation, based on differential testing: fuzzing arbitrary queries against a large schema, and testing a corpus of customer schemas against a comprehensive query.

Changes to the router's YAML configuration:

* The `experimental_introspection_mode` key has been removed, with the `new` mode as the only behavior in this release.
* The `supergraph.query_planning.legacy_introspection_caching` key is removed, with the behavior in this release now similar to what was `false`: introspection responses are not part of the query plan cache but instead in a separate, small in-memory—only cache.

Migrations ensure that existing configuration files will keep working.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/6139
