### Entity caching: do not cache data if age header is bigger than TTL ([PR #8456](https://github.com/apollographql/router/pull/8456))

Given this example of headers
```
Cache-Control: max-age=5
Age: 90
```
as `Age` is higher than `max-age` it should not cache the data because it’s already expired

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8456