### feat: allow JWTs without `exp` on a per-JWKS basis while still rejecting expired tokens ([Issue #8910](https://github.com/apollographql/router/issues/8910), [PR #8911](https://github.com/apollographql/router/pull/8911))

Adds a per-JWKS `allow_missing_exp` configuration option to Router JWT authentication. When enabled for a JWKS entry, tokens without an `exp` claim are accepted for that JWKS, while tokens that include an `exp` claim continue to be validated and rejected if expired.

This is useful for deployments that rely on long-lived machine-to-machine or service tokens that omit `exp`, without relaxing expiry validation globally.

By [@fernando-apollo](https://github.com/fernando-apollo) in https://github.com/apollographql/router/pull/TBD
