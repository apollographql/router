### Treat helm extra labels values as templates

When a helm chart users add extra labels to the Kubernetes Deployment and pods (via the helm chart), they may want to use a piece of data from the `Values` or the `Chart` eg using the Chart version as a label value.

At the moment doing something like:
```
extraLabels:
  env: {{ .Chart.AppVersion }}
```
is interpreted as:
```
labels:
  env: {{ .Chart.AppVersion }}
```

This is "normal" because by default strings are not interpolated. The fix basically iterates over the `extraLabels` and uses the `tpl` function on the values (only). The result being then
```
labels:
  env: "v1.2.3"
```

By [@gscheibel](https://github.com/gscheibel) in https://github.com/apollographql/router/pull/2962