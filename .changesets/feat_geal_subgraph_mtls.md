### TLS client authentication for subgraph requests ([Issue #3414](https://github.com/apollographql/router/issues/3414))

The router now supports TLS client authentication when connecting to subgraphs. It can be configured as follows:

```yaml
tls:
  subgraph:
    all:
      client_authentication:
        certificate_chain: ${file./path/to/certificate_chain.pem}
        key: ${file./path/to/key.pem}
    # if configuring for a specific subgraph:
    subgraphs:
      # subgraph name
      products:
        client_authentication:
          certificate_chain: ${file./path/to/certificate_chain.pem}
          key: ${file./path/to/key.pem}
```

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3794