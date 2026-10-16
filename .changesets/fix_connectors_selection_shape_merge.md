### Compute connector selection shapes in linear time ([PR #10404](https://github.com/apollographql/router/pull/10404))

Connectors validation and expansion compute the output shape of every connector's `selection`. The shapes of the fields of each object in the selection were merged one at a time, which rebuilt and rehashed the accumulated object shape for every field, so computing the shape of an object with N fields took O(N²) time. All the field shapes are now merged in a single pass. The computed shapes are unchanged.

Composing a subgraph with a single connector that selects 4,000 fields now takes 0.16s instead of 3.6s.

By [@dariuszkuc](https://github.com/dariuszkuc) in <https://github.com/apollographql/router/pull/10404>
