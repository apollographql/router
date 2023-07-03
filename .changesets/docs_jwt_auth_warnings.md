### Add a few security-related warnings to JWT auth docs ([PR #3299](https://github.com/apollographql/router/pull/3299))

There are a couple potential security pitfalls when leveraging the router for JWT authentication. These are now documented in [the relevant section of the docs](https://www.apollographql.com/docs/router/configuration/authn-jwt). If you are currently using JWT authentication in the router, be sure to [secure your subgraphs](https://www.apollographql.com/docs/federation/building-supergraphs/subgraphs-overview#securing-your-subgraphs) and [use care when propagating headers](https://www.apollographql.com/docs/router/configuration/authn-jwt#example-forwarding-claims-to-subgraphs). 

By [@dbanty](https://github.com/dbanty) in https://github.com/apollographql/router/pull/3299
