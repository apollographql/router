### Remove `deno_crypto` package due to security vulnerability ([Issue #5484](https://github.com/apollographql/router/issues/5484))

Removes [deno_crypto](https://crates.io/crates/deno_crypto) due to the vulnerability [reported in `curve25519-dalek`](https://rustsec.org/advisories/RUSTSEC-2024-0344 ).
Since the router exclusively used `deno_crypto` for generating UUIDs using the package's random number generator, this vulnerability had no impact on the router.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5483