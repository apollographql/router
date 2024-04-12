### Helm: include all standard labels in pod spec but complete sentence that stands on its own ([PR #4862](https://github.com/apollographql/router/pull/4862))

The templates for the router's Helm chart have been updated so that the  `helm.sh/chart`, `app.kubernetes.io/version`, and `app.kubernetes.io/managed-by` labels are now included on pods, as they already were for all other resources created by the Helm chart.

The specific change to the template is that the pod spec template now uses the `router.labels` template function instead of the `router.selectorLabels` template function. This allows you to remove a label from the selector without removing it from resource metadata by overriding the `router.selectorLabels` and `router.labels` functions and moving the label from the former to the latter.

By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/4862