### WIP: Connectors with TLS ([PR #6995](https://github.com/apollographql/router/pull/6995))

Connectors now supports TLS configuration for using custom certificate authorities and utilizing client certificate authentication.

```
tls:
  connector:
    sources:
      connector-graph.random_person_api:
        certificate_authorities: ${file.ca.crt}
        client_authentication:
          certificate_chain: ${file.client.crt}
          key: ${file.client.key}
```

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/6995
