### Support JWT tokens with multiple audiences ([PR #8780](https://github.com/apollographql/router/pull/8780))

When `issuers` or `audiences` is included in the router's JWK configuration, the router will check each request's JWT for `iss` or `aud` and reject requests with mismatches.

Expected behavior:
* If present, the [`iss`](https://datatracker.ietf.org/doc/html/rfc7519#section-4.1.1) claim must be specified as a **string**. 
  * ✅  the JWK's `issuers` is empty
  * ✅  the `iss` is a string and is present in the JWK's `issuers`
  * ✅  the `iss` is null
  * ❌  the `iss` is a string but is not present in the JWK's `issuers`
  * ❌  the `iss` is not a string or null
* If present, the [`aud`](https://datatracker.ietf.org/doc/html/rfc7519#section-4.1.3) claim can be specified as **either a string or an array of strings** .
  * ✅  the JWK's `audiences` is empty
  * ✅  the `aud` is a string and is present in the JWK's `audiences`
  * ✅  the `aud` is an array of strings and at least one of those strings is present in the JWK's `audiences`
  * ❌  the `aud` is not a string or array of strings (ie null)

Behavior prior to this change:
* If the `iss` was not null or a string, it would be permitted (regardless of its value)
* If the `aud` was an array, it would be rejected (regardless of its value)

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8780