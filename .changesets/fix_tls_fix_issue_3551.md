### TLS client configuration override for Redis ([Issue #3551](https://github.com/apollographql/router/issues/3551))

It is now possible to set up a client certificate or override the root certificate authority list for Redis connections, through the `tls` section under Redis configuration. Options follow the same format as [subgraph TLS configuration](https://www.apollographql.com/docs/router/configuration/overview/#tls):

```yaml
apq:
  router:
    cache:
      redis:
        urls: [ "redis://localhost:6379" ]
        tls:
          certificate_authorities: "${file./path/to/ca.crt}"
          client_authentication:
            certificate_chain: ${file./path/to/certificate_chain.pem}
            key: ${file./path/to/key.pem}
```



By [@geal](https://github.com/geal) in https://github.com/apollographql/router/pull/4304