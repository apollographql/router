### Improve JWKS parse error handling ([Issue #4463](https://github.com/apollographql/router/issues/4463))

When parsing a JWKS the Router should ignore any JWKs that fail to parse, rather than failing the entire JWKS parse. 
This can happen when the JWK is malformed, or when a JWK uses an unknown algorithm. When this happens a warning will be output to the logs, for example: 

```
2024-01-11T15:32:01.220034Z WARN fetch jwks{url=file:///tmp/jwks.json,}  ignoring a key since it is not valid, enable debug logs to full content err=unknown variant `UnknownAlg`, expected one of `HS256`, `HS384`, `HS512`, `ES256`, `ES384`, `RS256`, `RS384`, `RS512`, `PS256`, `PS384`, `PS512`, `EdDSA` alg="UnknownAlg" index=2
```

Log messages have the following attributes:
* `alg` The JWK algorithm if known or `<unknown>`
* `index` The index of the JWK within the JWKS.
* `url` The URL of the JWKS that had the issue.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4465
