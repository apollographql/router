### Fetch persisted query manifests from a standalone OCI artifact

When the router is configured with a persisted-queries artifact reference (`APOLLO_PQ_ARTIFACT_REFERENCE`), the persisted query manifest is read from that OCI artifact instead of Apollo Uplink, the same way the supergraph schema can already be read from a graph artifact reference. The artifact lives in its own repository and is fetched and polled independently of the schema image: either source can be moved to OCI without the other. The router polls the artifact tag (the variant's pointer, moved by persisted query publishes and list link changes) and assembles the manifest from the artifact's chunk layers. Chunk blobs are content-addressed and cached across polls, so a publish fetches only new or changed chunks.

A zero-layer artifact is an emptied list and yields an empty manifest, which in enforcing mode blocks all unregistered operations; an absent tag means no list is linked to the variant and surfaces as a fetch error rather than an empty manifest, so the two cannot be confused.

The persisted-queries reference follows the same rules as the schema reference: a tag is polled and requires `--hot-reload`, a digest is fetched once and requires it off. Both references share that flag, so a pointer tag pairs with a tag-referenced schema, and a digest-pinned schema with a digest-pinned persisted-queries artifact.

Local manifest files still take precedence, and routers without a persisted-queries artifact reference continue to use Uplink.

By [@samaanghani](https://github.com/samaanghani) in https://github.com/apollographql/router/pull/10238
