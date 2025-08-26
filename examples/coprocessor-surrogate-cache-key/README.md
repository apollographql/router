## Context

Existing caching systems often support a concept of surrogate keys, where a key can be linked to a specific piece of cached data, independently of the actual cache key.

As an example, a news website might want to invalidate all cached articles linked to a specific company or person following an event. To that end, when returning the article, the service can add a surrogate key to the article response, and the cache would keep a map from surrogate keys to cache keys.

## Surrogate keys and the router’s entity cache

To support a surrogate key system with the entity caching in the router, we make the following assumptions:

- The subgraph returns surrogate keys with the response. The router will not manipulate those surrogate keys directly. Instead, it leaves that task to a coprocessor
- The coprocessor tasked with managing surrogate keys will store the mapping from surrogate keys to cache keys. It will be useful to invalidate all cache keys related to a surrogate cache key in Redis.
- The router will expose a way to gather the cache keys used in a subgraph request

### Router side support

The router has two features to support surrogate cache key:

- An id field for subgraph requests and responses. This is a random, unique id per subgraph call that can be used to keep state between the request and response side, and keep data from the various subgraph calls separately for the entire client request. You have to enable it in configuration (`subgraph_request_id`):

```yaml title=router.yaml
coprocessor:
  url: http://127.0.0.1:3000 # mandatory URL which is the address of the coprocessor
  supergraph:
    response: 
      context: true
  subgraph:
    all:
      response: 
        subgraph_request_id: true
        context: true
```

- The entity cache has an option to store in the request context, at the key `apollo::entity_cache::cached_keys_status`, a map `subgraph request id => cache keys` only when it's enabled in the configuration (`expose_keys_in_context`)):

```yaml title=router.yaml
preview_entity_cache:
  enabled: true
  expose_keys_in_context: true
  metrics:
    enabled: true
  invalidation:
    listen: 0.0.0.0:4000
    path: /invalidation
  # Configure entity caching per subgraph
  subgraph:
    all:
      enabled: true
      # Configure Redis
      redis:
        urls: ["redis://localhost:6379"]
        ttl: 24h # Optional, by default no expiration
```

The coprocessor will then work at two stages:

- Subgraph response:
  - Extract the subgraph request id
  - Extract the list of surrogate keys from the response
- Supergraph stage:
  - Extract the map `subgraph request id => cache keys`
  - Match it with the surrogate cache keys obtained at the subgraph response stage

The coprocessor then has a map of `surrogate keys => cache keys` that it can use to invalidate cached data directly from Redis.

### Example workflow

- The router receives a client request
- The router starts a subgraph request:
  - The entity cache plugin checks if the request has a corresponding cached entry:
    - If the entire response can be obtained from cache, we return a response here
    - If it cannot be obtained, or only partially (\_entities query), a request is transmitted to the subgraph
  - The subgraph responds to the request. The response can contain a list of surrogate keys in a header: `Surrogate-Keys: homepage, feed`
  - The subgraph response stage coprocessor extracts the surrogate keys from headers, and stores it in the request context, associated with the subgraph request id `0e67db40-e98d-4ad7-bb60-2012fb5db504`:

```json
{
  "​0ee3bf47-5e8d-47e3-8e7e-b05ae877d9c7": ["homepage", "feed"]
}
```

- The entity cache processes the subgraph response:
    - It generates a new subgraph response by interspersing data it got from cache with data from the original response
    - It stores the list of keys in the context. `new` indicates newly cached data coming from the subgraph, linked to the surrogate keys, while `cached` is data obtained from the cache. These are the keys directly used in Redis:

```json
{
  "apollo::entity_cache::cached_keys_status": {
    "0ee3bf47-5e8d-47e3-8e7e-b05ae877d9c7": [
      {
        "key": "version:1.0:subgraph:products:type:Query:hash:af9febfacdc8244afc233a857e3c4b85a749355707763dc523a6d9e8964e9c8d:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c",
        "status": "new",
        "cache_control": "max-age=60,public"
      }
    ]
  }
}
```

- The supergraph response stage loads data from the context and creates the mapping:

```json
{
  "homepage": [
    {
      "key": "version:1.0:subgraph:products:type:Query:hash:af9febfacdc8244afc233a857e3c4b85a749355707763dc523a6d9e8964e9c8d:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c",
      "status": "new",
      "cache_control": "max-age=60,public"
    }
  ],
  "feed": [
    {
      "key": "version:1.0:subgraph:products:type:Query:hash:af9febfacdc8244afc233a857e3c4b85a749355707763dc523a6d9e8964e9c8d:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c",
      "status": "new",
      "cache_control": "max-age=60,public"
    }
  ]
}
```

- When a surrogate key must be used to invalidate data, that mapping is used to obtained the related cache keys


In this example we provide a very simple implementation using in memory data in NodeJs. It just prints the mapping at the supergraph response level to show you how you can create that mapping.
