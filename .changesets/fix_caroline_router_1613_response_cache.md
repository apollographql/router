### Correct no-store and no-cache behavior for response cache and entity cache plugins ([PR #8948](https://github.com/apollographql/router/pull/8948) and [PR #8952](https://github.com/apollographql/router/pull/8952))

Confusingly, `no-store` and `no-cache` have different meanings. Per [RFC 9111](https://datatracker.ietf.org/doc/html/rfc9111#name-cache-control):
* `no-store`: allows serving response from cache, but prohibits storing response in cache
* `no-cache`: prohibits serving response from cache, but allows storing response in cache
(NB: `no-cache` actually prohibits serving response from cache _without revalidation_ - but the router doesn't distinguish between lookup and revalidation)

The response caching and entity caching plugins were incorrectly treating `no-store` as both 'no serving response from the cache' and 'no storing response in the cache.'

This change fixes that behavior.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8948 and https://github.com/apollographql/router/pull/8952
