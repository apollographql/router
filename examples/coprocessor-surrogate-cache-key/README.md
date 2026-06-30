## Context

Existing caching systems often support a concept of surrogate keys, where a key can be linked to a specific piece of cached data, independently of the actual cache key.

As an example, a news website might want to invalidate all cached articles linked to a specific company or person following an event. To that end, when returning the article, the service can add a surrogate key to the article response, and the cache would keep a map from surrogate keys to cache keys.

## Surrogate keys and the router

To support a surrogate key system with response caching in the router, we make the following assumptions:

- The subgraph returns surrogate keys with the response. The router will not manipulate those surrogate keys directly. Instead, it leaves that task to a coprocessor
- The coprocessor tasked with managing surrogate keys will store the mapping from surrogate keys to cache keys. It will be useful to invalidate all cache keys related to a surrogate cache key in Redis.
- The router will expose a way to gather the cache keys used in a subgraph request

### Router side support

The router has a unique id field for subgraph requests and responses. This is a random, unique id per subgraph call that can be used to keep state between the request and response side, and keep data from the various subgraph calls separately for the entire client request.

The response cache can expose cache key details in the request context, at the key `apollo::response_cache::debug_cached_keys`, when debug mode is enabled:

```yaml title=router.yaml
response_cache:
  enabled: true
  debug: true
  subgraph:
    all:
      enabled: true
      redis:
        urls: ["redis://localhost:6379"]
        ttl: 24h # Optional, by default no expiration
```

The coprocessor will then work at two stages:

- Subgraph response:
  - Extract the list of surrogate keys from the response headers
  - Record the mapping of subgraph name to surrogate keys
- Supergraph stage:
  - Read the list of cache entries from `apollo::response_cache::debug_cached_keys` in the context
  - Match surrogate keys (obtained at the subgraph response stage) to the corresponding cache keys by subgraph name

The coprocessor then has a map of `surrogate keys => cache keys` that it can use to invalidate cached data directly from Redis.

### Example workflow

- The router receives a client request
- The router starts a subgraph request:
  - The subgraph responds to the request. The response can contain a list of surrogate keys in a header: `Surrogate-Keys: homepage, feed`
  - The subgraph response stage coprocessor extracts the surrogate keys from headers, and stores them keyed by subgraph name:

```json
{
  "products": ["homepage", "feed"]
}
```

- The supergraph response stage reads `apollo::response_cache::debug_cached_keys` from the context, which is a list of cache entry objects:

```json
[
  {
    "key": "version:1.2:subgraph:products:type:Query:hash:af9febfacdc8244afc233a857e3c4b85a749355707763dc523a6d9e8964e9c8d:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c",
    "subgraphName": "products",
    "source": "subgraph",
    "shouldStore": true,
    "invalidationKeys": ["currentUser"],
    "cacheControl": { "maxAge": 60, "public": true }
  }
]
```

- The coprocessor matches each entry's `subgraphName` against the stored surrogate keys to create the final mapping:

```json
{
  "homepage": [
    "version:1.2:subgraph:products:type:Query:hash:af9febfacdc8244afc233a857e3c4b85a749355707763dc523a6d9e8964e9c8d:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c"
  ],
  "feed": [
    "version:1.2:subgraph:products:type:Query:hash:af9febfacdc8244afc233a857e3c4b85a749355707763dc523a6d9e8964e9c8d:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c"
  ]
}
```

- When a surrogate key must be used to invalidate data, that mapping is used to obtain the related cache keys


In this example we provide a very simple implementation using in memory data in NodeJs. It just prints the mapping at the supergraph response level to show you how you can create that mapping.
