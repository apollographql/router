### Include hostname on heaptrack path, specify pod lifecycle ([Issue #5789](https://github.com/apollographql/router/issues/5789))

Use hostname in the heaptrack path to identify an individual container/instance/machine where the router is running. This means the filepath for heaptrack output will change.

Additionally, allow the specification of restartPolicy on deployment (defaults to `Always` which is kubernetes default)

By [@cyberhck](https://github.com/cyberhck) in https://github.com/apollographql/router/pull/5850
