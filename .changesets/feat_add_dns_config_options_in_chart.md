### Set the router pod's DNS configuration from the Helm chart ([PR #10295](https://github.com/apollographql/router/pull/10295))

The router Helm chart has a new `dnsConfig` value that sets the [pod DNS config](https://kubernetes.io/docs/concepts/services-networking/dns-pod-service/#pod-dns-config) on the router Deployment.
For example, lowering `ndots` means external hostnames are not tried against every cluster search domain first:

```yaml
dnsConfig:
  options:
    - name: ndots
      value: "2"
```

The value is empty by default, so you don't need to change anything unless you want to set it.

By [@cyberhck](https://github.com/cyberhck) in https://github.com/apollographql/router/pull/10295
