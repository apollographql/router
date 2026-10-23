### Query deduplication uses less CPU per subgraph request

With `traffic_shaping` `deduplicate_query` enabled, the router now spends about 70% less CPU checking each subgraph query for an identical request already in flight. In a micro-benchmark, each subgraph fetch took about 0.65 µs instead of 2.1 µs on one thread, and about 1 µs instead of 9 µs with four threads sending to the same subgraph at once. Operations that make many subgraph fetches save this on each one. No configuration change is needed.

By [@rohan-b99](https://github.com/rohan-b99)
