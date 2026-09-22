### Fail fast on ambiguous license-source config

The router already refused to start if its config ambiguously mixed Graph Artifacts and
Uplink for its _schema_ source, failing with a clear error instead of guessing. License
configuration had no equivalent protection: setting both an explicit license (`--license` /
`APOLLO_ROUTER_LICENSE_PATH`, or the literal `APOLLO_ROUTER_LICENSE` value) and a graph
artifact reference (`--graph-artifact-reference` / `APOLLO_GRAPH_ARTIFACT_REFERENCE`) would
silently pick the explicit license and ignore the graph artifact reference.

The router now mirrors the schema-source check: if an explicit license (path or literal) and
a graph artifact reference are both configured, it fails fast at startup with a clear error
instead of silently picking one. Studio credentials (`APOLLO_KEY` / `APOLLO_GRAPH_REF`)
overlapping with a graph artifact reference is unaffected — that combination still resolves
to the OCI registry, since `APOLLO_KEY` may simply be needed to authenticate the OCI pull.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/####
