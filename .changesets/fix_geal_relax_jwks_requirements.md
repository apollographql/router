### Relax JWKS requirements ([PR #4234](https://github.com/apollographql/router/pull/4234))

Previously in the Apollo Router's logic for validating JWT with a corresponding JWK, a bug occured when the `use` and `key_ops` JWK parameters were absent, resulting in the key not being selected for verification. This bug has been fixed in this release.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4234