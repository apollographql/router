### Remove legacy validation ([PR #4551](https://github.com/apollographql/router/pull/4551))

GraphQL query validation was initially performed by the query planner, which resulted in some performance issues. We designed a new validation process using ApolloCompiler from the apollo-rs project, that runs earlier in the request handling pipeline, at the router service level instead of the supergraph service.
That validtion system has been running in production for months concurrently with the Javascript version, to detect any discrepancy between their behaviors. This built enough confidence in the new implementation that we are now entirely moving to it and removing the Javascript validation.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4551