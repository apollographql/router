### Query deduplication ignores the subgraph operation name

With `traffic_shaping` `deduplicate_query` enabled, subgraph requests that are identical except for their operation name now share one in-flight request. The query planner gives every fetch its own operation name, so fetches within a single client request that sent the same query and variables to the same subgraph were never deduplicated, even though their responses are identical. This happens, for example, when the same entity is reached through several paths in the response.

Subgraph request hooks in coprocessors, Rhai scripts, and plugins run once for each shared request and see the operation name of the first request that started it.

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/10465
