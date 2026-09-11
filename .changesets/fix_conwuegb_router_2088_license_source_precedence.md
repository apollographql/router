### Fix license-source precedence to key on the graph artifact reference, not Studio credentials ([PR #10117](https://github.com/apollographql/router/pull/10117))

At startup, the router decided whether to fetch its license from the OCI graph artifact registry or from Apollo Uplink by checking whether Apollo Studio credentials (`APOLLO_KEY`/`APOLLO_GRAPH_REF`) were set, instead of checking whether a graph artifact reference (`--graph-artifact-reference` / `APOLLO_GRAPH_ARTIFACT_REFERENCE`) was configured. This broke two scenarios:

- A standard router (Studio credentials set, no graph artifact reference) was incorrectly routed to the OCI registry for its license, which failed at startup since no artifact reference was configured.
- A self-hosted router pointed at its own OCI registry, with no Studio credentials, required Studio credentials to even be considered for OCI, and silently fell through to no license source at all.

License-source resolution now mirrors the existing schema-source precedence: the OCI registry is used whenever a graph artifact reference is configured, falling back to Apollo Uplink otherwise.

By [@conwuegb](https://github.com/conwuegb) in https://github.com/apollographql/router/pull/10117
