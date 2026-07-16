### Enforce a kid-specific JWKS entry's constraints instead of falling through to a less-specific entry

When two JWKS entries shared the same key material — one kid-specific and constrained by `issuer`/`audience`, the other algorithm-only and unconstrained — a token that should have been rejected by the constrained entry could be accepted through the unconstrained one. When searching for a key, a kid match and an algorithm match each scored equally, so a kid-specific entry (matched on kid only) and an alg-only entry (matched on algorithm only) tied and were both returned as candidates. Validation then verified the signature against the constrained entry, failed its issuer/audience check, and fell through to the unconstrained entry, which accepted the token.

When a token's key ID (`kid`) matches one or more JWKS entries, only those matching entries are now considered, and each is validated in turn. A token can no longer be accepted by falling through to an entry whose `kid` it never matched. Entries that share key material under the same `kid` but carry different constraints — for example, the same key duplicated across a multi-tenant identity provider — are all still tried, so legitimate multi-entry setups continue to work.

By [@carodewig](https://github.com/carodewig)
