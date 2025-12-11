### Response caching is now Generally Available 🎉 ([PR #8678](https://github.com/apollographql/router/pull/8678))

**Response caching is now Generally Available (GA)** and ready for production use!

Response caching enables the router to cache subgraph query responses using Redis, improving query latency and reducing load on your underlying services. Unlike traditional HTTP caching solutions, response caching provides GraphQL-aware caching at the entity and root field level, making cached data reusable across different users and queries.

For complete documentation, configuration options, and quickstart guide, see the [response caching documentation](https://www.apollographql.com/docs/graphos/routing/performance/caching/response-caching/overview).

#### Key benefits

- **Improved performance**: Cache origin responses and reuse them across queries to reduce latency
- **Reduced subgraph load**: Minimize redundant requests to your subgraphs by serving cached data
- **Entity-level caching**: Cache individual entity representations independently, enabling fine-grained control over data freshness
- **Flexible cache control**: Set different TTLs for different types of data based on `@cacheControl` directives or `Cache-Control` response headers
- **Privacy-aware**: Share cached data across users while maintaining privacy for personalized data
- **Active cache invalidation**: Tag cached data with `@cacheTag` and invalidate specific cache entries via HTTP endpoint when data changes

#### What's cached

The router caches two kinds of data:
- **Root query fields**: Cached as complete units (the entire response for these root fields)
- **Entity representations**: Cached independently—each origin's contribution to an entity is cached separately and can be reused across different queries

#### Additional features

- **Cache debugger**: See exactly what's being cached during development
- **Redis cluster support**: Scale your cache with Redis cluster deployments and read replicas
- **Comprehensive metrics**: Monitor cache performance with detailed Redis-specific metrics

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8678
