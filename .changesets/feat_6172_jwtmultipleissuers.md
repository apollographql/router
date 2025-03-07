### Allow JWT authorization options to support multiple issuers ([Issue #6172](https://github.com/apollographql/router/issues/6172))

Allow JWT authorization options to support multiple issuers using the same jwks, similar to the support found on other tech stacks/frameworks.

Configuration change:

Any `issuer` defined on each currently existing `jwks` needs to migrated to an entry in the `issuers` list.  

example:
https://www.npmjs.com/package/jsonwebtoken
> issuer (optional): string or array of strings of valid values for the iss field.

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/6887
