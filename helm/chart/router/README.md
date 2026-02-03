# router

[router](https://github.com/apollographql/router) Rust Graph Routing runtime for Apollo Federation

![Version: 2.3.0](https://img.shields.io/badge/Version-2.3.0-informational?style=flat-square) ![Type: application](https://img.shields.io/badge/Type-application-informational?style=flat-square) ![AppVersion: v2.3.0](https://img.shields.io/badge/AppVersion-v2.3.0-informational?style=flat-square)

## Prerequisites

* Kubernetes v1.19+

## Get Repo Info

```console
helm pull oci://ghcr.io/apollographql/helm-charts/router --version 2.3.0
```

## Install Chart

**Important:** only helm3 is supported

```console
helm upgrade --install [RELEASE_NAME] oci://ghcr.io/apollographql/helm-charts/router --version 2.3.0 --values my-values.yaml
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
| containerPorts.http | int | `4000` | If you override the port in `router.configuration.server.listen` then make sure to match the listen port here |
| containerPorts.metrics | int | `9090` | For exposing the metrics port when running a serviceMonitor for example |
| extraContainers | list | `[]` | An array of extra containers to include in the router pod Example: extraContainers:   - name: coprocessor     image: acme/coprocessor:1.0     ports:       - containerPort: 4001 |
| extraEnvVars | list | `[]` | An array of extra environmental variables Example: extraEnvVars:   - name: APOLLO_ROUTER_SUPERGRAPH_PATH     value: /etc/apollo/supergraph.yaml   - name: APOLLO_ROUTER_LOG     value: debug  |
| extraEnvVarsCM | string | `""` | ConfigMap name containing additional environment variables |
| extraEnvVarsSecret | string | `""` | Secret name containing sensitive environment variables |
| extraLabels | object | `{}` | A map of extra labels to apply to the resources created by this chart Example: extraLabels:   label_one_name: "label_one_value"   label_two_name: "label_two_value" |
| extraVolumeMounts | list | `[]` | An array of extra VolumeMounts Example: extraVolumeMounts:   - name: rhai-volume     mountPath: /dist/rhai     readonly: true |
| extraVolumes | list | `[]` | An array of extra Volumes Example: extraVolumes:   - name: rhai-volume     configMap:       name: rhai-config  |
| fullnameOverride | string | `""` |  |
| image.pullPolicy | string | `"IfNotPresent"` |  |
| image.repository | string | `"ghcr.io/apollographql/router"` |  |
| image.tag | string | `""` |  |
| imagePullSecrets | list | `[]` | Array of imagePullSecret references for private container registries |
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
| managedFederation.existingSecret | string | `nil` | If using managed federation, use an existing Kubernetes Secret which stores the graph API key instead of creating a new one. If set along with `managedFederation.apiKey`, a secret with the graph API key will be created using this parameter as the secret name |
| managedFederation.existingSecretKeyRefKey | string | `nil` | If using managed federation, the name of the key within the existing Secret which stores the graph API key. If set along with `managedFederation.apiKey`, a secret with the graph API key will be created using this parameter as the key name, defaults to `managedFederationApiKey` |
| managedFederation.graphRef | string | `""` | If using managed federation, the variant of which graph to use |
| nameOverride | string | `""` |  |
| nodeSelector | object | `{}` |  |
| podAnnotations | object | `{}` | Annotations to add to the router pod |
| podDisruptionBudget | object | `{}` | Sets the [pod disruption budget](https://kubernetes.io/docs/tasks/run-application/configure-pdb/) for Deployment pods |
| podSecurityContext | object | `{}` | Pod-level security context settings |
| priorityClassName | string | `""` | Set to existing PriorityClass name to control pod preemption by the scheduler |
| probes.liveness | object | `{"initialDelaySeconds":0}` | Configure liveness probe |
| probes.readiness | object | `{"initialDelaySeconds":0}` | Configure readiness probe |
| replicaCount | int | `1` |  |
| resources | object | `{}` |  |
| restartPolicy | string | `"Always"` | Sets the restart policy of pods |
| rollingUpdate | object | `{}` | Sets the [rolling update strategy parameters](https://kubernetes.io/docs/concepts/workloads/controllers/deployment/#rolling-update-deployment). Can take absolute values or percentage values. |
| router | object | `{"args":["--hot-reload"],"configuration":{"health_check":{"listen":"0.0.0.0:8088"},"supergraph":{"listen":"0.0.0.0:4000"}}}` | For YAML structure, see https://www.apollographql.com/docs/graphos/routing/configuration/yaml |
| securityContext | object | `{}` | Container-level security context settings |
| service.annotations | object | `{}` | Annotations to add to the Service (e.g., for load balancer configuration) |
| service.port | int | `80` |  |
| service.targetPort | string | `"http"` |  |
| service.type | string | `"ClusterIP"` |  |
| serviceAccount.annotations | object | `{}` |  |
| serviceAccount.create | bool | `true` |  |
| serviceAccount.name | string | `""` |  |
| serviceMonitor.enabled | bool | `false` |  |
| serviceentry.enabled | bool | `false` |  |
| supergraphFile | string | `nil` |  |
| terminationGracePeriodSeconds | int | `30` | Sets the [termination grace period](https://kubernetes.io/docs/concepts/containers/container-lifecycle-hooks/#hook-handler-execution) for Deployment pods |
| tolerations | list | `[]` |  |
| topologySpreadConstraints | list | `[]` | Sets the [topology spread constraints](https://kubernetes.io/docs/concepts/scheduling-eviction/topology-spread-constraints/) for Deployment pods |
| virtualservice.enabled | bool | `false` |  |

