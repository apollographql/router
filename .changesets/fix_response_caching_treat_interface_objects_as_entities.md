### Treat interface objects as entities in response caching ([PR #8582](https://github.com/apollographql/router/pull/8582))

Interface objects can be entities, but response caching wasn't treating them that way. Interface objects are now respected as entities so they can be used as cache keys.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/8582
