### Response caching is now Generally Available 🎉 ([PR #8678](https://github.com/apollographql/router/pull/8678))

We're excited to announce that **response caching is now Generally Available (GA)** and ready for production use!

Response caching is a powerful feature that enables the router to cache subgraph query responses using Redis, dramatically improving query latency and reducing load on your underlying services. Unlike traditional HTTP caching solutions, response caching provides GraphQL-aware caching at the entity and root field level, making cached data reusable across different users and queries.

#### Key benefits

- **Improved performance**: Cache origin responses and reuse them across queries to reduce latency
- **Reduced subgraph load**: Minimize redundant requests to your subgraphs by serving cached data
- **Entity-level caching**: Cache individual entity representations independently, enabling fine-grained control over data freshness
- **Flexible cache control**: Set different TTLs for different types of data based on `@cacheControl` directives in your schema or the `Cache-Control` response header
- **Privacy-aware**: Mix public and private data safely—share cached data across users while maintaining privacy for personalized data
- **GraphQL-native**: Solve unique GraphQL caching challenges that traditional CDNs can't address, such as mixed TTLs and high data duplication
- **Active cache invalidation**: Use the `@cacheTag` directive in your schema to tag cached data, then actively invalidate specific cache entries via an HTTP endpoint when data changes

#### What's cached

The router caches two kinds of data:
- **Root query fields**: Cached as complete units (the entire response for these root fields)
- **Entity representations**: Cached independently—each origin's contribution to an entity is cached separately and can be reused across different queries


#### Configuration

Response caching uses Redis as the cache backend and can be configured per subgraph with invalidation support:

```yaml
response_cache:
  enabled: true
  # Configure the invalidation endpoint
  invalidation:
    listen: 127.0.0.1:4000
    path: /invalidation
  subgraph:
    all:
      enabled: true
      ttl: 30s
      redis:
        urls:
          - redis://127.0.0.1:6379
      invalidation:
        enabled: true
        shared_key: ${env.INVALIDATION_SHARED_KEY}
```

The router determines cache TTLs from `Cache-Control` HTTP headers returned by your origins. You also get comprehensive Redis metrics to monitor cache performance, including connection health, command execution, and operational insights.

#### Cache invalidation with @cacheTag

Tag your cached data using the `@cacheTag` directive in your subgraph schema, then actively invalidate specific cache entries when data changes:

```graphql
type Query {
  user(id: ID!): User @cacheTag(format: "user-{$args.id}")
  users: [User!]! @cacheTag(format: "users-list")
}

type User @key(fields: "id") @cacheTag(format: "user-{$key.id}") {
  id: ID!
  name: String!
}
```

Send invalidation requests to remove cached data before TTL expires:

```bash
curl --request POST \
	--header "authorization: $INVALIDATION_SHARED_KEY" \
	--header 'content-type: application/json' \
	--url http://localhost:4000/invalidation \
	--data '[{"kind":"cache_tag","subgraphs":["posts"],"cache_tag":"user-42"}]'
```

#### Additional features

- **Cache debugger**: See exactly what's being cached during development with detailed cache key inspection
- **Redis cluster support**: Scale your cache with Redis cluster deployments and benefit from read replicas for improved performance and availability
- **Comprehensive metrics**: Monitor cache performance with detailed Redis-specific metrics including connection health, command execution, and latency

For complete documentation, configuration options, and best practices, see the [response caching documentation](/router/routing/performance/caching/response-caching/overview).

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8678
