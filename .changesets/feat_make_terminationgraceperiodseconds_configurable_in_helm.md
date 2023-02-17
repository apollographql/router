### Make terminationGracePeriodSeconds property configurable in the Helm chart

`terminationGracePeriodSeconds` is now configurable on the Deployment object in Helm chart.

This is useful & recommended to adjust if you are changing the default timeout on the router, and should always be a value slightly bigger than the timeout in order to ensure no requests are closed prematurely on shutdown.

By [@Meemaw](https://github.com/Meemaw) in https://github.com/apollographql/router/pull/2582