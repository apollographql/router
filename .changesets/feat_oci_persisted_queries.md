### Fetch persisted query manifests from the graph artifact OCI image

When the router is configured with a graph artifact reference (`APOLLO_GRAPH_ARTIFACT_REFERENCE`), persisted query manifests are now read from the graph artifact image itself instead of Apollo Uplink, the same way the supergraph schema already is. The router polls the image tag and assembles the manifest from the image's persisted query chunk layers, so a persisted query publish becomes visible via the same artifact that delivers the schema. Chunk blobs are content-addressed and cached across polls: a schema-only launch refetches no persisted query data, and a persisted query publish fetches only new or changed chunks.

Local manifest files still take precedence, and routers without a graph artifact reference continue to use Uplink.

By [@samaanghani](https://github.com/samaanghani) in https://github.com/apollographql/router/pull/10238
