### Reject GET requests with a non-`application/json` Content-Type header ([GHSA-hff2-gcpx-8f4p](https://github.com/apollographql/router/security/advisories/GHSA-hff2-gcpx-8f4p))

The router now rejects GraphQL `GET` requests that include a `Content-Type` header with a value other than `application/json` (with optional parameters such as `; charset=utf-8`). Any other value is rejected with a 415 status code.

`GET` requests without a `Content-Type` header continue to be allowed (subject to the router's existing [CSRF prevention](/router/configuration/csrf) check), since `GET` requests have no body and therefore technically do not require this header.

This improvement makes the router's CSRF prevention more resistant to browsers that implement CORS in non-spec-compliant ways. Apollo is aware of one browser which as of March 2026 has a bug allowing an attacker to circumvent the router's CSRF prevention to carry out read-only XS-Search-style attacks. The browser vendor is in the process of patching this vulnerability; upgrading to this version of the router mitigates the vulnerability.

**If your graph uses cookies (or HTTP Basic Auth) for authentication, Apollo encourages you to upgrade to this version.**

This is technically a backwards-incompatible change. Apollo is not aware of any GraphQL clients that provide non-empty `Content-Type` headers on `GET` requests with types other than `application/json`. If your use case requires such requests, please contact support, and we may add more configurability in a follow-up release.

(This is a backport of a change from v2.12.1. This fix is not part of Router v2.11.0 through v2.12.0.)

By [@glasser](https://github.com/glasser) and [@carodewig](https://github.com/carodewig) in [GHSA-hff2-gcpx-8f4p](https://github.com/apollographql/router/security/advisories/GHSA-hff2-gcpx-8f4p)