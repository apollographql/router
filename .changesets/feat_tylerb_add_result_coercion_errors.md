### Response reformatting and result coercion errors ([PR #8441](https://github.com/apollographql/router/pull/8441))

All subgraph responses are checked and corrected to ensure alignment with the
schema and query. When a misaligned value is returned, it is nullified. Now,
when the feature is enabled, an errors for this nullification are included in
the response.

By [@TylerBloom](https://github.com/TylerBloom) in https://github.com/apollographql/router/pull/8441
