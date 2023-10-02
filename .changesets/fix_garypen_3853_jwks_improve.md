### Ignore JWKS keys which aren't supported by the router ([Issue #3853](https://github.com/apollographql/router/issues/3853))

If you have a JWKS which contains a key which has an algorithm (alg) which the router doesn't recognise, then the entire JWKS is disregarded. This is unsatisfactory, since there are likely to be many other keys in the JWKS which the router could use.

We have changed the JWKS processing logic so that we remove entries with an unrecognised algorithm from the list of available keys. We print a warning with the name of the algorithm for each removed entry.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3922
