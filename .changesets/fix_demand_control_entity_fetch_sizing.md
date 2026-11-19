### Size `_entities` fetches by their parent lists in demand control estimates ([PR #9192](https://github.com/apollographql/router/pull/9192))

When estimating the cost of a federated operation, demand control sized every entity fetch (`_entities`) with the configured `list_size`, regardless of how many objects the fetch extends. The estimate was too high when an entity fetch extends a single object, and too low when it extends objects in nested lists.

An entity fetch is now sized by the number of objects at the response path it extends, which is the product of the list sizes along that path. Those list sizes are the ones the rest of the estimate already uses: `@listSize` (`assumedSize`, `slicingArguments`, and `sizedFields`), then the `list_size` configured for the subgraph that returns the list, then the global `list_size`.

For example, with `list_size: 10` and fields that each need an entity fetch to another subgraph:

| Operation | Entities fetched, before | Entities fetched, now |
| --- | --- | --- |
| `product { inStock }` | 10 | 1 |
| `companies { employees { salary } }` | 10 | 100 (10 companies × 10 employees) |
| `products(first: 3) { inStock }`, where `products` has `@listSize(slicingArguments: ["first"])` | 10 | 3 |

Estimated costs for operations with entity fetches can go up as well as down, so review your configured `max` values before upgrading if you run demand control in `enforce` mode.

This fix doesn't change the following behaviors of the estimate:

- A list without `@listSize` is still assumed to have `list_size` items.
- Costs are summed across fragments on different object types, even though an object can only match one of them. A field selected in several such fragments is counted once per fragment.
- `@skip` and `@include` are only applied when their condition is a literal. A condition given as a variable is ignored, and the field is counted.

By [@duckki](https://github.com/duckki) and [@sachindshinde](https://github.com/sachindshinde) in https://github.com/apollographql/router/pull/9192
