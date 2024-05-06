### Use `subgraph.name` attribute instead of `apollo.subgraph.name` ([PR #5012](https://github.com/apollographql/router/pull/5012))

In the router v1.45.0, subgraph name mapping didn't work correctly in the Datadog exporter.

The Datadog exporter does some explicit mapping of attributes and was using a value `apollo.subgraph.name` that the latest versions of the router don't use. The correct choice is `subgraph.name`.

This release updates the mapping to reflect the change and fixes subgraph name mapping for Datadog.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/5012
