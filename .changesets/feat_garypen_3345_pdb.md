### Add support for PodDisruptionBudget to helm chart ([Issue #3345](https://github.com/apollographql/router/issues/3345))

A [PodDisuptionBudget](https://kubernetes.io/docs/tasks/run-application/configure-pdb/) may now be specified for your router to limit the number of concurrent disruptions.

Example Configuration:

```yaml
podDisruptionBudget:
  minAvailable: 1
```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3469