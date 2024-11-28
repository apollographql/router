### Fix the query hashing algorithm ([PR #6205](https://github.com/apollographql/router/pull/6205))

The Router includes a schema-aware query hashing algorithm designed to return the same hash across schema updates if the query remains unaffected. This update enhances the algorithm by addressing various corner cases, improving its reliability and consistency.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/6205kkk
