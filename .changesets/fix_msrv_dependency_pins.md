### Pin dependencies to maintain Rust 1.85.0 MSRV ([PR #TBD](https://github.com/apollographql/router/pull/TBD))

Pin several transitive dependencies to versions compatible with our current MSRV of Rust 1.85.0, preventing automatic upgrades that would require newer Rust versions.

**Pinned dependencies:**

- `aws-config` to `1.5.4` (prevents upgrade to 1.8+ which requires AWS SDK packages needing Rust 1.88)
- `aws-sdk-sso` to `1.54.0` (MSRV 1.81.0)
- `aws-sdk-ssooidc` to `1.55.0` (MSRV 1.81.0)  
- `aws-sdk-sts` to `1.55.0` (MSRV 1.81.0)
- `aws-smithy-types` to `1.2.12` (newer 1.3.x requires tokio ^1.40.0)
- `aws-smithy-runtime-api` to `1.7.3` (1.9+ requires tokio ^1.40.0)
- `aws-smithy-async` to `1.2.4` (maintains compatibility with aws-sdk-sso while avoiding tokio >=1.40)
- `async-compression` to `0.4.11` (0.4.12+ requires tokio ^1.40.0)
- `home` to `0.5.9` (MSRV 1.70.0)
- `async-graphql` / `async-graphql-axum` to `7.0.10` in fuzz subgraph (avoids 1.86.0 requirement from 7.0.15+)
- `async-graphql-derive`, `async-graphql-parser`, `async-graphql-value` to `7.0.10` (transitive deps must match to avoid macro incompatibilities)
- `apollo-smith` to `0.15.0` (0.15.2+ uses unstable feature `unsigned_is_multiple_of` requiring Rust 1.87+)

**Critical tokio version constraint:**

The AWS Smithy and async-compression pins enforce `tokio 1.38.2` — versions >=1.40 introduce stricter blocking file descriptor restrictions that cause `opentelemetry-jaeger 0.19.0` to panic at runtime (see tokio-rs/tokio#7172). Until we upgrade the entire OpenTelemetry stack (currently 0.20.0) to a jaeger-compatible version, we must keep tokio <1.40.0 via these transitive dependency pins.

**Trade-offs:**

These pins reduce our ability to receive automatic security updates for these dependencies — we'll need to monitor for critical fixes and manually bump when necessary. The AWS SDK versions are several releases behind latest (1.54-1.55 vs 1.92+), though they still provide comfortable headroom above our MSRV (requiring only 1.81).

We can selectively bump to mid-range versions (e.g., `aws-sdk-sso` 1.74.0) to narrow the gap while staying under any future MSRV increases, pending verification of functional compatibility.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/TBD
