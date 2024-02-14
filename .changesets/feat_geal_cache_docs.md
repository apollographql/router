### GraphOS entity caching ([Issue #4478](https://github.com/apollographql/router/issues/4478))

> ⚠️ This is a preview for an [Enterprise feature](https://www.apollographql.com/blog/platform/evaluating-apollo-router-understanding-free-and-open-vs-commercial-features/) of the Apollo Router. It requires an organization with a [GraphOS Enterprise plan](https://www.apollographql.com/pricing/).
>
> If your organization doesn't currently have an Enterprise plan, you can test out this functionality by signing up for a free Enterprise trial.

The Apollo Router can now cache fine-grained subgraph responses at the entity level, which are reusable between requests.

Caching federated GraphQL responses can be done at the HTTP level, but it's inefficient because a lot of data can be shared between different requests. The Apollo Router now contains an entity cache that works at the subgraph level: it caches subgraph responses, splits them by entities, and reuses entities across subgraph requests.
Along with reducing the cache size, the router's entity cache brings more flexibility in how and what to cache. It allows the router to store different parts of a response with different expiration dates, and link the cache with the authorization context to avoid serving stale, unauthorized data.

Because it's a preview feature, the entity cache isn't production ready. It doesn't support cache invalidation. We're making it available to test and gather feedback.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4195