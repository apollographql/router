### Helm: Enable easier Kubernetes debugging with heaptrack ([Issue #5789](https://github.com/apollographql/router/issues/5789))

The router's Helm chart has been updated to help make debugging with heaptrack easier.

Previously, when debugging multiple Pods with heaptrack, all Pods wrote to the same file, so they'd overwrite each others' results. This issue has been fixed by adding a `hostname` to each output data file from heaptrack.

Also, the Helm chart now supports a `restartPolicy` that enables you to configure a Pod's [restart policy](https://kubernetes.io/docs/concepts/workloads/pods/pod-lifecycle/#container-restarts). The default value of `restartPolicy` is `Always` (the same as the Kubernetes default).


By [@cyberhck](https://github.com/cyberhck) in https://github.com/apollographql/router/pull/5850
