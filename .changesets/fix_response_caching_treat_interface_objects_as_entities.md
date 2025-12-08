### Fixes response caching by treating interface objects as entities

Interface objects can be entities but response caching wasn't treating them that way. This fix makes sure they're respected as entities so that they can be used as cache keys.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/8582
