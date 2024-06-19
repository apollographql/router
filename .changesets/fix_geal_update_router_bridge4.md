### Security fix: Timing variability in curve25519-dalek ([PR #5483](https://github.com/apollographql/router/pull/5483))

This removes deno_crypto due to the vulnerability reported in curve25519-dalek: https://rustsec.org/advisories/RUSTSEC-2024-0344 
The router is not affected by that vulnerability, as deno_crypto was only used to provide a random number generator to create UUIDs

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5483