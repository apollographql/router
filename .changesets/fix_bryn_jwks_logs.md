### Improve logging for JWKS download failures ([Issue #4448](https://github.com/apollographql/router/issues/4448))

To enable users to debug JWKS download and parse failures more easily, we've added more detailed logging to the router. The router now logs the following information when a JWKS download or parse fails:

```
2024-01-09T12:32:20.174144Z ERROR fetch jwks{url=http://bad.jwks.com/,}  could not create JSON Value from url content, enable debug logs to see content e=expected value at line 1 column 1
```
Enabling debug logs via `APOLLO_LOG=debug` or `--logs DEBUG` will show the full JWKS content being parsed:
```
2024-01-09T12:32:20.153055Z DEBUG fetch jwks{url=http://bad.jwks.com/,}  parsing JWKS data="invalid jwks"
```

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4449
