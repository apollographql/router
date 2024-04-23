### Entity cache preview: support queries with private scope ([PR #4855](https://github.com/apollographql/router/pull/4855))

**This is part of the work on [subgraph entity caching](https://www.apollographql.com/docs/router/configuration/entity-caching/), currently in preview.**

This adds support for caching responses marked with the `private` scope. For now, this is meant to work  only from what the subgraph is returning, without any schema level information.

this encodes the following behaviour:

- if the subgraph does not return `Cache-Control: private` for a query, then it works as usual
- if the subgraph would return `Cache-Control: private` for a query:
  - if we have seen this query before:
    - if we have configured a `private_id` option and the related data is found in context provided by the authentication plugin, then we look for the cache entry for this query, by adding a hash of the `private_id` to the cache key. If not present, we perform the query, and then store the data at this personalized cache key
    - if there is no `private_id`, the we do not cache the response because we have no way to distinguish users
  - if it is the first time we see this query:
    - first we look into the cache  for the basic cache key (without the `private_id` hash) because we cannot know in advance if it requires private data
    - there should not be a cached entry since we have not seen this query before. Another router instance could have seen it?
    - we send the request to the subgraph and get back a response with `Cache-Control: private`
    - add the query to the list of known private queries
    - if we have a `private_id\:
      - update the cache key to add the hash of the sub claim
      - store the response in cache
    - if there is no `private_id`: since we don't have a way to differentiate users, then we do not cache the response

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4855