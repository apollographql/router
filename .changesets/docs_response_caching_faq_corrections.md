### Correct response caching documentation for schema updates and multi-root-field caching

Updated the response caching FAQ to accurately describe caching behavior:

- Clarified that schema updates generate new cache keys, so old entries won't receive cache hits (effectively expired from the user's perspective) rather than implying stale data might be served.
- Fixed the explanation of multi-root-field caching to correctly state that the router caches the entire subgraph response as a single unit, not separately per root field.
- Added clarification that the configured TTL is a fallback when subgraph responses don't include `Cache-Control: max-age` headers.
- Changed example TTL from `300s` to `5m` for better readability.

By [@the-gigi-apollo](https://github.com/the-gigi-apollo) in https://github.com/apollographql/router/pull/8794
