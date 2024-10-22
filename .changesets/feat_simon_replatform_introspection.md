### Remove legacy schema introspection ([PR #6139](https://github.com/apollographql/router/pull/6139))

Schema introspection in the router now runs natively without JavaScript. We have high confidence that the new native implementation returns responses that match the previous Javascript implementation, based on differential testing: fuzzing arbitrary queries against a large schema, and testing a corpus of customer schemas against a comprehensive query.

Changes to the router's YAML configuration:

* The `experimental_introspection_mode` key has been removed, with the `new` mode as the only behavior in this release.
* The `supergraph.query_planning.legacy_introspection_caching` key is removed, with the behavior in this release now similar to what was `false`: introspection responses are not part of the query plan cache but instead in a separate, small in-memory—only cache.

When using the above deprecated configuration options, the router's automatic configuration migration will ensure that existing configuration continue to work until the next major version of the router.  To simplify major upgrades, we recommend reviewing incremental updates to your YAML configuration by comparing the output of `./router config upgrade --config path/to/config.yaml` with your existing configuration.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/6139
