### move AllowOnlyHttpPostMutationsLayer at the supergraph service level ([PR #3374](https://github.com/apollographql/router/pull/3374), [PR #3410](https://github.com/apollographql/router/pull/3410))

Now that we have access to a compiler in supergraph requests, we don't need to look into the query plan to know if a request contains mutations

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3374 & https://github.com/apollographql/router/pull/3410