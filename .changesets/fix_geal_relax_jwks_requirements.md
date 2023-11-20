### Relax JWKS requirements ([PR #4234](https://github.com/apollographql/router/pull/4234))

We had a bug where if `use` and `key_ops` were absent, the key was not selected for verification

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4234