### Use the entire schema when hashing an introspection query ([Issue #5006](https://github.com/apollographql/router/issues/5006))

in https://github.com/apollographql/router/pull/4883 (1.44.0), we introduced a query hashing scheme that stays stable across schema updates if the update does not affect the query. Unfortunately, it was not taking introspection queries into account.
This fixes the hashing mechanism to add the schema string to hashed data if we encounter an introspection field, so the entire schema is taken into account

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5007