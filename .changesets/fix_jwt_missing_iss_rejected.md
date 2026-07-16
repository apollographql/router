### Reject JWTs that omit the `iss` claim when a JWKS entry configures an `issuers` allowlist

When a JWKS entry is configured with an `issuers` allowlist, the router now rejects a validly-signed JWT that omits its `iss` claim (or sets it to `null`), instead of accepting it. Previously a token could sidestep the allowlist simply by not carrying an issuer, even though a token presenting a non-matching issuer was already rejected.

By [@carodewig](https://github.com/carodewig)