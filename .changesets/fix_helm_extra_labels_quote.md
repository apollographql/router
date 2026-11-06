### Helm chart quotes `extraLabels` values so non-string-looking values render as strings ([Issue #7204](https://github.com/apollographql/router/issues/7204))

The Helm chart rendered each `extraLabels` value without quotes. A value such as `"true"` or `"123"` therefore became a YAML boolean or number in the rendered manifest, and Kubernetes rejected it because label values must be strings:

```
contains non-string key in the map: true is of the type bool, expected string
```

The chart now quotes every rendered `extraLabels` value, so `somelabel: "true"` renders as `somelabel: "true"`. Template expressions in values, such as `{{ .Chart.AppVersion }}`, still work.

If you worked around this by double-quoting the value (for example, `somelabel: '"true"'`), remove the extra quotes. Otherwise the label value contains literal double quotes, which Kubernetes rejects.

By [@RaphaelFakhri](https://github.com/RaphaelFakhri) in https://github.com/apollographql/router/pull/10768
