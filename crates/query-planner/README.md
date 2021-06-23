# Query planner
Query planning implementations and model are in this crate.

## Implementations
* Harmonizer - calls out to the node.js version of the query planner via [deno](https://deno.land).
* Caching - a decorator that adds caching to any query planner.
* TODO - native rust query planner.

Usage:

```rust
let mut planner = HarmonizerQueryPlanner::new(schema).with_caching();
let result = planner.get(query, operation, QueryPlanOptions::default());
```

## Model
The query planner model consists of a tree of nodes:
* Fetch - perform a query to a subgraph.
* Sequence - child nodes are executed in order.
* Parallel - child nodes may be executed in parallel.
* Flatten - merge child nodes with the final result.
