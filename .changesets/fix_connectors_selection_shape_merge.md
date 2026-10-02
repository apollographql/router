### Compute connector selection shapes in linear time

Connectors validation and expansion compute the output shape of every connector's `selection`. The shapes of the fields of each object in the selection were merged one at a time, which rebuilt and rehashed the accumulated object shape for every field, so computing the shape of an object with N fields took O(N²) time. All the field shapes are now merged in a single pass. The computed shapes are unchanged.

On a supergraph with 17 subgraphs and 1,543 connectors, composition drops from 51s to 33s: connectors validation from 27s to 18s, and connectors expansion from 22s to 13s.

By [@dariuszkuc](https://github.com/dariuszkuc)
