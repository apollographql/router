### Make the satisfiability check far cheaper for supergraphs that use Apollo Connectors ([PR #9663](https://github.com/apollographql/router/pull/9663))

Composing a supergraph that uses Apollo Connectors could consume large amounts of memory and time in the **satisfiability check**, the step that verifies every field in the API schema can actually be resolved.

The check expands every `@connect` into its own internal subgraph, and how far that fans out depends on the schema. In one pathological graph — far larger fan-out than anything else we've seen — 6 connector subgraphs expanded into roughly 1,400 internal ones, pushing the check to about **14 GB and 4 minutes**.

Many of those internal subgraphs are interchangeable for this check: same way in, same data. Composition now merges the interchangeable ones **only for the satisfiability check**, so it runs over a far smaller graph. On that same graph the check dropped to about **0.3 GB and under a second**, with an identical pass/fail result. The schema the router runs on is untouched, so query planning and connector execution are unchanged.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/9663
