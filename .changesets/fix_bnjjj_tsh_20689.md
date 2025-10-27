### Prevent entity caching of expired data based on Age header ([PR #8456](https://github.com/apollographql/router/pull/8456))

When the `Age` header is higher than the `max-age` directive in `Cache-Control`, the router no longer caches the data because it's already expired.

For example, with these headers:
```
Cache-Control: max-age=5
Age: 90
```
The data won't be cached since `Age` (90) exceeds `max-age` (5).

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8456