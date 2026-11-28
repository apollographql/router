### Fetch the entitlement under the graph artifact's path prefix ([PR #10790](https://github.com/apollographql/router/pull/10790))

When a graph artifact reference points at a registry that mounts Apollo's repositories under a path prefix, such as a pull-through cache configured with a destination prefix (`mirror.internal/apollo/<graph>`), the router read the entitlement identifier from the graph artifact's manifest but then fetched the entitlement from `entitlements/<id>` at the registry root, dropping the prefix. The mirror has nothing mounted there, so the fetch returned 404 on every poll and the router never licensed itself.

The entitlement is now fetched from `<prefix>/entitlements/<id>`, next to the graph repository, so prefixed mirrors work. References without a prefix are unchanged.

By [@samaanghani](https://github.com/samaanghani) in https://github.com/apollographql/router/pull/10790
