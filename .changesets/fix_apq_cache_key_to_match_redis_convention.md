### Replace null separator in cache key with `:` to match Redis convention ([PR #4886](https://github.com/apollographql/router/pull/4886))

To conform with Redis convention, the router now uses `:` instead of null as the separator in cache keys. This conformance helps to properly display cache keys in nested form in Redis clients. 

This PR (#4886) updates the separator for APQ cache keys. Another PR (#4583) updates the separator for query plan cache keys.

By [@tapaderster](https://github.com/tapaderster) in https://github.com/apollographql/router/pull/4886