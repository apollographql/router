### Upgrade to Rust 1.71.1 ([PR #3536](https://github.com/apollographql/router/pull/3536))

This includes the fix for [CVE-2023-38497](https://blog.rust-lang.org/2023/08/03/cve-2023-38497.html).

We’re applying the upgrade as a precaution, but we don’t have any shared multi-user environments which  build the Router (whether developer workstations or other environments). This CVE would only affect users who were building the Router themselves using Cargo on such shared multi-user machines and wouldn’t affect our published binaries, the use of our Docker images, etc.

Users building custom binaries should consider their own build environments to determine if they were impacted.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/3536
