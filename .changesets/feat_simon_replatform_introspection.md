### Remove JavaScript-based schema introspection ([PR #6139](https://github.com/apollographql/router/pull/6139))

Schema introspection now runs natively without involving JavaScript code. We have high confidence that the two implementations return matching responses based on differential testing: fuzzing arbitrary queries against a large schema, and testing a corpus of customer schemas against a comprehensive query.

In Router YAML configuration:

* The `experimental_introspection_mode` key is removed, `new` is now the only behavior
* The `supergraph.query_planning.legacy_introspection_caching` key is removed, the behavior is now similar to what was `false`: introspection responses are not part of the query plan cache but instead in a separate, small, in-memory only cache.

Migrations ensure that existing configuration files will keep working.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/6139
