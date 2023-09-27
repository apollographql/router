### Ignore JWKS keys with an algorithm (alg) of ES512 ([Issue #3853](https://github.com/apollographql/router/issues/3853))

If you have a JWKS which contains a Key which has an algorithm (alg) which the router doesn't recognise, then the entire JWKS is disregarded. This is unsatisfactory, since there are likely to be many other keys in the JWKS which the router could use.

We have changed the JWKS processing logic so that keys which we know aren't supported by the router are excluded from the set of keys we use.

Currently we exclude any keys with an algorithm "ES512", this may change in the future.

We also print a warning message at router startup to let you know we are doing this.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3922