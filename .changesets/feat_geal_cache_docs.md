### Preview for GraphOS Entity caching ([Issue #4478](https://github.com/apollographql/router/issues/4478))

> ⚠️ This is a preview for an [Enterprise feature](https://www.apollographql.com/blog/platform/evaluating-apollo-router-understanding-free-and-open-vs-commercial-features/) of the Apollo Router. It requires an organization with a [GraphOS Enterprise plan](https://www.apollographql.com/pricing/).
>
> If your organization doesn't currently have an Enterprise plan, you can test out this functionality by signing up for a free Enterprise trial.

While federated GraphQL responses can be cached at the HTTP level, this is actually wasteful, as a lot of data can actually be shared between different requests. The Apollo Router now contains an entity cache, that works at the subgraph level: it is able to cache subgraph responses, splitting them by entities, and reusing entities across subgraph requests.
Along with reducing the cache size, this brings more flexibility in how and what to cache, allowing us to store different parts of a response with different expiration dates, and linking the cache with the authorization context, to avoid serving stale unauthorized data.

This is a preview feature, not ready yet fort production usage, as it is missing significant parts like invalidation, but we make it available now for testing and to gather feedback.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4195