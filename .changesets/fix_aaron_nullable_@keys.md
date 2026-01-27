### Support nullable `@key` fields in response caching ([PR #8767](https://github.com/apollographql/router/pull/8767))

Response caching can now use nullable `@key` fields. Previously, the response caching feature rejected nullable `@key` fields, which prevented caching in schemas that use them.

When you cache data keyed by nullable fields, keep your cache keys simple and avoid ambiguous `null` values.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/8767
