### Copy less of the response while executing query plans ([PR #PULL_NUMBER](https://github.com/apollographql/router/pull/PULL_NUMBER))

Query plan execution no longer copies data it only needs to read:

- A `Sequence` node returns only the data its fetches added, instead of a copy of the whole response built so far. Before, a `Sequence` nested in a `Parallel` node deep-copied the response and merged it back into the response twice, so its cost grew with the size of the response rather than with what it fetched.
- Fetches share their fetch node and the paths of their entity representations instead of deep-copying them.
- Deduplicating entity representations hashes each representation once instead of twice, using a faster hasher.
- Response formatting only records the paths of nulled values when the operation uses `@defer`, the only case that reads them. It also stops building a new type for every non-null value and for every object with a `__typename`.

Responses are unchanged. In the new `nested_sequence` benchmark, a query over 1,000 products whose plan nests a `Sequence` in a `Parallel` node takes about 11% less time per request.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/PULL_NUMBER
