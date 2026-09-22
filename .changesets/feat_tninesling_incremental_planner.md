### Add incremental query planner ([PR #10100](https://github.com/apollographql/router/pull/10100), [PR #10101](https://github.com/apollographql/router/pull/10101), [PR #10102](https://github.com/apollographql/router/pull/10102), [PR #10103](https://github.com/apollographql/router/pull/10103), [PR #10104](https://github.com/apollographql/router/pull/10104), [PR #10105](https://github.com/apollographql/router/pull/10105), [PR #10106](https://github.com/apollographql/router/pull/10106))

Introduces an incremental query planner based on the BULB (Budgeted Unlimited Lookahead Breadth-first) search algorithm. The planner explores the query graph to find optimal fetch plans by routing fields across subgraphs. This is an internal implementation not yet exposed to users. Key components include:

- BULB search over query graph edges with configurable beam width and lookahead budget
- FetchGraph with undo-log rollback for speculative fetch node construction
- SharedPath and SelectionBuilder for efficient selection set construction
- Field routing enumeration with type explosion and fragment restructuring placeholders
- Plan builder for materializing FetchGraph nodes into executable fetch plans

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/10100
