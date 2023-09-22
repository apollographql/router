# router

[router](https://github.com/apollographql/router) Rust Graph Routing runtime for Apollo Federation

![Version: 1.30.1](https://img.shields.io/badge/Version-1.30.1-informational?style=flat-square) ![Type: application](https://img.shields.io/badge/Type-application-informational?style=flat-square) ![AppVersion: v1.30.1](https://img.shields.io/badge/AppVersion-v1.30.1-informational?style=flat-square)

## Prerequisites

* Kubernetes v1.19+

## Get Repo Info

```console
helm pull oci://ghcr.io/apollographql/helm-charts/router --version 1.30.1
```

## Install Chart

**Important:** only helm3 is supported

```console
helm upgrade --install [RELEASE_NAME] oci://ghcr.io/apollographql/helm-charts/router --version 1.30.1 --values my-values.yaml
```

_See [configuration](#configuration) below._

## Configuration

See [Customizing the Chart Before Installing](https://helm.sh/docs/intro/using_helm/#customizing-the-chart-before-installing). To see all configurable options with detailed comments, visit the chart's [values.yaml](./values.yaml), or run these configuration commands:

```console
helm show values oci://ghcr.io/apollographql/helm-charts/router
```

## Values

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| affinity | object | `{}` |  |
| autoscaling.enabled | bool | `false` |  |
| autoscaling.maxReplicas | int | `100` |  |
| autoscaling.minReplicas | int | `1` |  |
| autoscaling.targetCPUUtilizationPercentage | int | `80` |  |
| containerPorts.health | int | `8088` | For exposing the health check endpoint |
| containerPorts.http | int | `80` | If you override the port in `router.configuration.server.listen` then make sure to match the listen port here |
| containerPorts.metrics | int | `9090` | For exposing the metrics port when running a serviceMonitor for example |
| extraContainers | list | `[]` | An array of extra containers to include in the router pod Example: extraContainers:   - name: coprocessor     image: acme/coprocessor:1.0     ports:       - containerPort: 4001 |
| extraEnvVars | list | `[]` |  |
| extraEnvVarsCM | string | `""` |  |
| extraEnvVarsSecret | string | `""` |  |
| extraLabels | object | `{}` | A map of extra labels to apply to the router deploment and containers Example: extraLabels:   label_one_name: "label_one_value"   label_two_name: "label_two_value" |
| extraVolumeMounts | list | `[]` |  |
| extraVolumes | list | `[]` |  |
| fullnameOverride | string | `""` |  |
| image.pullPolicy | string | `"IfNotPresent"` |  |
| image.repository | string | `"ghcr.io/apollographql/router"` |  |
| image.tag | string | `""` |  |
| imagePullSecrets | list | `[]` |  |
| ingress.annotations | object | `{}` |  |
| ingress.className | string | `""` |  |
| ingress.enabled | bool | `false` |  |
| ingress.hosts[0].host | string | `"chart-example.local"` |  |
| ingress.hosts[0].paths[0].path | string | `"/"` |  |
| ingress.hosts[0].paths[0].pathType | string | `"ImplementationSpecific"` |  |
| ingress.tls | list | `[]` |  |
| initContainers | list | `[]` | An array of init containers to include in the router pod Example: initContainers:   - name: init-myservice     image: busybox:1.28     command: ["sh"] |
| lifecycle | object | `{}` |  |
| managedFederation.apiKey | string | `nil` | If using managed federation, the graph API key to identify router to Studio |
| managedFederation.existingSecret | string | `nil` | If using managed federation, use existing Secret which stores the graph API key instead of creating a new one. If set along `managedFederation.apiKey`, a secret with the graph API key will be created using this parameter as name |
| managedFederation.graphRef | string | `""` | If using managed federation, the variant of which graph to use |
| nameOverride | string | `""` |  |
| nodeSelector | object | `{}` |  |
| podAnnotations | object | `{}` |  |
| podDisruptionBudget | object | `{}` | Sets the [pod disruption budget](https://kubernetes.io/docs/tasks/run-application/configure-pdb/) for Deployment pods |
| podSecurityContext | object | `{}` |  |
| priorityClassName | string | `""` | Set to existing PriorityClass name to control pod preemption by the scheduler |
| probes.liveness | object | `{"initialDelaySeconds":0}` | Configure liveness probe |
| probes.readiness | object | `{"initialDelaySeconds":0}` | Configure readiness probe |
| replicaCount | int | `1` |  |
| resources | object | `{}` |  |
| router | object | `{"args":["--hot-reload"],"configuration":{"health_check":{"listen":"0.0.0.0:8088"},"supergraph":{"listen":"0.0.0.0:80"},"telemetry":{"metrics":{"prometheus":{"enabled":false,"listen":"0.0.0.0:9090","path":"/metrics"}}}}}` | See https://www.apollographql.com/docs/router/configuration/overview/#yaml-config-file for yaml structure |
| securityContext | object | `{}` |  |
| service.annotations | object | `{}` |  |
| service.port | int | `80` |  |
| service.type | string | `"ClusterIP"` |  |
| serviceAccount.annotations | object | `{}` |  |
| serviceAccount.create | bool | `true` |  |
| serviceAccount.name | string | `""` |  |
| serviceMonitor.enabled | bool | `false` |  |
| serviceentry.enabled | bool | `false` |  |
| supergraphFile | string | `nil` |  |
| terminationGracePeriodSeconds | int | `30` | Sets the [termination grace period](https://kubernetes.io/docs/concepts/containers/container-lifecycle-hooks/#hook-handler-execution) for Deployment pods |
| tolerations | list | `[]` |  |
| virtualservice.enabled | bool | `false` |  |

----------------------------------------------
Autogenerated from chart metadata using [helm-docs v1.11.0](https://github.com/norwoodj/helm-docs/releases/v1.11.0)
