### Declare the router's artifacts in `router.yaml` with the `registry` section

The graph artifact, the persisted-queries artifact and the entitlement can now be declared in one place in `router.yaml` instead of one environment variable per artifact:

```yaml
registry:
  host: artifact.api.apollographql.com
  artifacts:
    graph:
      reference: my-graph:production
    persisted_queries:
      reference: persistedqueries/my-graph:production
```

Every slot is a repository under one registry host and path prefix, so a pull-through cache or mirror of Apollo's registry is declared once (`host` and `prefix`) and applies to every artifact, including the entitlement, which is fetched from `entitlements/<id>` next to the graph repository under the same prefix. `auth` picks how the router authenticates (`auto`, `apollo_key`, `docker_credentials`, `anonymous`), and `poll_interval` and `insecure` apply to every slot.

The section follows configuration reloads: editing a reference while the router runs restarts only the poller it changes, so pointing `artifacts.graph` at another registry fetches that schema without a restart, and slots that did not change keep their pollers and cached state.

`APOLLO_GRAPH_ARTIFACT_REFERENCE` keeps working and overrides `artifacts.graph`; when both are set they must name the same registry. The persisted-queries artifact has no environment variable and is declared here only.

By [@samaanghani](https://github.com/samaanghani) in https://github.com/apollographql/router/pull/TBD
