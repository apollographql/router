### Ignore JWKS keys which aren't in the algorithms configuration ([Issue #3853](https://github.com/apollographql/router/issues/3853))

If you have a JWKS which contains a Key which has an algorithm (alg) which the router doesn't recognise, then the entire JWKS is disregarded. This is unsatisfactory, since there are likely to be many other keys in the JWKS which the router could use.

We have changed the JWKS processing logic so that we use the list of `algorithms` specified in your configuration to exclude keys which are not contained in that list.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3922
