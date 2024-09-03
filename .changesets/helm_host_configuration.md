### Allow for configuration of the host via the helm template for virtual service ([PR #5545](https://github.com/apollographql/router/pull/5795))

Using the virtual service template change allows teh configuration of the host from a variable when doing helm deploy.
The default of any host causes issues for those that use different hosts for a single AKS cluster

By [@nicksephora](https://github.com/nicksephora) in https://github.com/apollographql/router/pull/5545
