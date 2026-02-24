### Correct response caching FAQ for schema updates and multi-root-field caching ([PR #8794](https://github.com/apollographql/router/pull/8794))

Updated the response caching FAQ to accurately describe caching behavior:

- Clarify that schema updates generate new cache keys, so old entries don't receive cache hits (effectively expired from your perspective) instead of implying stale data might be served.
- Correct the multi-root-field caching explanation to state that the router caches the entire subgraph response as a single unit, not separately per root field.
- Add clarification that the configured TTL is a fallback when subgraph responses don't include `Cache-Control: max-age` headers.
- Change example TTL from `300s` to `5m` for better readability.

By [@the-gigi-apollo](https://github.com/the-gigi-apollo) in https://github.com/apollographql/router/pull/8794
