### refactor: place the `include_subgraph_errors` and `headers` layers explicitly in the pipeline ([PR #10091](https://github.com/apollographql/router/pull/10091))

The `include_subgraph_errors` and `headers` plugins previously reached each stage through the `Plugin` wrap hooks, folded into every stage in one global registry order. Their behaviour is now exposed as named layer constructors placed directly in `pipeline/stages.rs`, so each stage's composition reads as ordinary source code. This is an internal implementation change with no effect on router behavior, configuration, or responses.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/10091
