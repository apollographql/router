### Document that license entitlements are not backfilled into existing Graph Artifacts

The Router 3 upgrade guide now explains that the license identifier is only added to the manifest of Graph Artifacts built after the feature is available (and only when enabled for the graph). Existing artifacts are not backfilled, so a router that fetches an older artifact runs without a license rather than falling back to Apollo Uplink. The guide describes how to resolve this by rebuilding the artifact (or, for self-hosted OCI registries, supplying an offline license, which can't be combined with an Apollo-hosted artifact reference), and how to check the resolved license source.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/10401
