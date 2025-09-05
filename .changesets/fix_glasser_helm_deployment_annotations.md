### Enable annotations on deployments via Helm Chart ([PR #8164](https://github.com/apollographql/router/pull/8164))

The Helm chart previously did not allow customization of annotations on the deployment itself (as opposed to the pods within it, which is done with `podAnnotations`); this can now be done with the `deploymentAnnotations` value.

By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/8164