### fix(authentication): retry JWKS candidates on issuer/audience mismatch ([PR #9214](https://github.com/apollographql/router/pull/9214))

When multiple JWKS entries share identical key material (e.g., Azure AD B2C multi-policy tenants where different policies use the same RSA key), the router now correctly retries validation against all matching candidates. Previously, issuer and audience validation happened after the candidate loop, so a token that passed signature verification against the first JWKS entry would be rejected if that entry's configured issuer or audience didn't match — with no attempt to try the remaining entries.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9214
