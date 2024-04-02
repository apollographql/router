### Helm: include all standard labels in pod spec but complete sentence that stands on its own ([PR #4862](https://github.com/apollographql/router/pull/4862))

The `helm.sh/chart`, `app.kubernetes.io/version`, and `app.kubernetes.io/managed-by` labels are now included on pods just like they are in every other resource created by the Helm chart, because the pod spec template now uses the `router.labels` template function instead of the `router.selectorLabels` template function. This also means that you can remove a label from the selector without removing it from resource metadata by overriding the `router.selectorLabels` and `router.labels` functions and moving the label from the former to the latter.


By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/4862