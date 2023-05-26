### Evaluate multiple keys matching a JWT criteria ([Issue #3017](https://github.com/apollographql/router/issues/3017))

In some cases, multiple keys could match what a JWT asks for (both the algorithm, `alg`, and optional key identifier, `kid`). Previously, we scored each possible match and only took the one with the highest score. But even then, we could have multiple keys with the same score (e.g., colliding `kid` between multiple JWKS in tests).

The improved behavior will:

- Return a list of those matching `key` instead of the one with the highest score.
- Try them one by one until the JWT is validated, or return an error.
- If some keys were found with the highest possible score (matching `alg`, with `kid` present and matching, too), then we only test those keys.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3031
