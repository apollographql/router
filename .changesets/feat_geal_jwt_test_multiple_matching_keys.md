### Test multiple keys matching a JWT criteria ([Issue #3017](https://github.com/apollographql/router/issues/3017))

In some cases, multiple keys can match what a JWT asks (algorithm and optional kid). Previously, we scored each possible match and only took the one with the highest score. But even then, we can have multiple keys with the same score (example: colliding kid between multiple JWKS in tests).
This changes the behaviour to:
- return a list of matching key instead of the one with the highest score
- try them one by one until the JWT is validated, or return an error
- if some keys were found with the highest possible score (matching alg, kid is present and matching too), then we only test those ones

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3031