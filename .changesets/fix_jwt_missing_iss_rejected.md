### Reject JWTs that omit the `iss` claim when a JWKS entry configures an `issuers` allowlist

When a JWKS entry is configured with an `issuers` allowlist, the router now rejects a validly-signed JWT that omits its `iss` claim (or sets it to `null`), instead of accepting it. Previously a token could sidestep the allowlist simply by not carrying an issuer, even though a token presenting a non-matching issuer was already rejected.

This matches how the router already handles the `aud` claim under an `audiences` allowlist: once an operator configures an allowlist, a token that cannot satisfy it is rejected. Behavior is unchanged when no issuers are configured — tokens with or without an `iss` claim are still accepted.

By [@carodewig](https://github.com/carodewig)
