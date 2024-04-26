### Use entire schema when hashing an introspection query ([Issue #5006](https://github.com/apollographql/router/issues/5006))

A query hashing scheme introduced in the router [v1.44.0](https://github.com/apollographql/router/pull/4883) to enable stable hashes across schema updates (if updates didn't affect queries) unfortunately didn't take into account introspection queries.

This release fixes the hashing mechanism by adding the schema string to hashed data if an introspection field is encountered. As a result, the entire schema is taken into account.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5007