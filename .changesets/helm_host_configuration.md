### Allow configuring host via Helm template for virtual service ([PR #5545](https://github.com/apollographql/router/pull/5795))

When deploying via Helm, you can now configure hosts in `virtualservice.yaml` as a single host or a range of hosts. This is helpful when different hosts could be used within a cluster.

By [@nicksephora](https://github.com/nicksephora) in https://github.com/apollographql/router/pull/5545
