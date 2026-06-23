### Fix `Cache-Control` parsing and serialization in the response cache ([Issue #ROUTER-1830](https://apollographql.atlassian.net/browse/ROUTER-1830))

The response cache's `Cache-Control` handling has been refactored and several bugs fixed:

- **`stale-if-error=N` parse error fixed**: Subgraph responses containing `stale-if-error=600` previously caused a `SUBREQUEST_HTTP_ERROR`. The directive is now stored as `Option<u64>` and parsed correctly.
- **Rolling-upgrade serde compatibility**: Old Redis entries that stored `stale-if-error` or `stale-while-revalidate` as a boolean are now transparently deserialized instead of failing.
- **Extension-only `Cache-Control` headers**: A header containing only unrecognized extension directives (e.g. `cdn-cache-control=300`) is now treated as `no-store` rather than being cached indefinitely with no TTL.
- **`s-maxage` preserved separately from `max-age`** throughout parsing, merging, and serialization.
- **Extension directive values**: Directives whose values contain `=` (e.g. `cdn-cache-control=rev=abc`) are now correctly passed through to the `_ => {}` wildcard instead of returning a parse error, per RFC 9111 §5.2.
- **`no-cache` field-specific form**: `no-cache="Authorization"` (RFC 9111 §5.2.2.4) now correctly permits caching rather than being treated as a blanket revalidation directive.
- **Clock skew**: Cache entries whose `created` timestamp is in the future are now treated as expired.
- **`public`/`private` mutual exclusion**: The response serializer now correctly suppresses `public` when `private` is also set.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9562
