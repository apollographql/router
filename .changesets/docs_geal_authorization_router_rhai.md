### GraphOS authorization: add an example of scope manipulation with router service level rhai ([PR #3719](https://github.com/apollographql/router/pull/3719))

The router authorization directive `@requiresScopes` expects scopes to come from the `scope` claim in the OAuth2 access token format ( https://datatracker.ietf.org/doc/html/rfc6749#section-3.3 ). Some tokens may have scopes stored in a different way, like an array of strings, or even in different claims. This documents a way to extract the scopes and prepare them in the right format for consumption by `@requiresScopes`, ushing Rhai.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3719