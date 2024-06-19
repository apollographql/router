### Security fix: timing variability in curve25519-dalek ([Issue #5484](https://github.com/apollographql/router/issues/5484))

This removes deno_crypto due to the vulnerability reported in curve25519-dalek: https://rustsec.org/advisories/RUSTSEC-2024-0344 
The router is not affected by that vulnerability, as deno_crypto was only used to provide a random number generator to create UUIDs

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5483