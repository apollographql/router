### add support for JWT audience validation ([PR #7578](https://github.com/apollographql/router/pull/7578))

Adds support for validating the `aud` (audience) claim in JWTs. This allows the router to ensure that the JWT is intended
for the specific audience it is being used with, enhancing security by preventing token misuse across different audiences.

#### Example Usage

```yaml title="router.yaml"
authentication:
 router:
   jwt:
     jwks: # This key is required.
       - url: https://dev-zzp5enui.us.auth0.com/.well-known/jwks.json
         issuers: # optional list of issuers
           - https://issuer.one
           - https://issuer.two
         audiences: # optional list of audiences
           - https://my.api
           - https://my.other.api
         poll_interval: <optional poll interval>
         headers: # optional list of static headers added to the HTTP request to the JWKS URL
           - name: User-Agent
             value: router
     # These keys are optional. Default values are shown.
     header_name: Authorization
     header_value_prefix: Bearer
     on_error: Error
     # array of alternative token sources
     sources:
       - type: header
         name: X-Authorization
         value_prefix: Bearer
       - type: cookie
         name: authz
```

By [@Velfi](https://github.com/Velfi) in https://github.com/apollographql/router/pull/7578
