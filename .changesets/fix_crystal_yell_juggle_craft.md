### Order HPA target so that kubernetes does not rewrite ([Issue #4435](https://github.com/apollographql/router/issues/4435))

This update addresses an OutOfSync issue in ArgoCD applications when Horizontal Pod Autoscaler (HPA) is configured with both memory and CPU limits.
Previously, the live and desired manifests within Kubernetes were not consistently sorted, leading to persistent OutOfSync states in ArgoCD.
This change implements a sorting mechanism for HPA targets within the Helm chart, ensuring alignment with Kubernetes' expected order.
This fix proactively resolves the sync discrepancies while using HPA, circumventing the need to wait for Kubernetes' issue resolution (kubernetes/kubernetes#74099).

By [@cyberhck](https://github.com/cyberhck) in https://github.com/apollographql/router/pull/4436
