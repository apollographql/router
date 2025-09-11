### Add validation and metrics when fetching the schema from the Apollo Graph Artifact Registry ([PR #8081](https://github.com/apollographql/router/pull/8081))

Rename the `GRAPH_ARTIFACT_REFERENCE` configuration environment variable to `APOLLO_GRAPH_ARTIFACT_REFERENCE` for consistency and added validation.

Adds `apollo.router.artifact.fetch.count.total` and `apollo.router.artifact.fetch.duration.seconds` metrics.

By [@graytonio](https://github.com/graytonio) and [@sirdodger](https://github.com/sirdodger) in https://github.com/apollographql/router/pull/8081
