### Fetch the router entitlement from the OCI registry

Routers configured with a graph artifact reference (`APOLLO_GRAPH_ARTIFACT_REFERENCE`) now fetch their entitlement from the same OCI registry as their schema, instead of Apollo Uplink. The router discovers the account's entitlement identifier from the graph artifact manifest annotations and polls the entitlement artifact with the same graph API key. License verification and enforcement are unchanged. A manifest without the annotation leaves the router unlicensed until the graph is republished, and a missing entitlement artifact is retried quietly; revocation always arrives as a new license, never as an absence.

By [@samaanghani](https://github.com/samaanghani) in https://github.com/apollographql/router/pull/10159
