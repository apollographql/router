### Use subgraph.name attribute not apollo.subgraph.name ([PR #5012](https://github.com/apollographql/router/pull/5012))

The Datadog exporter does some explicit mapping of attributes and was using a value "apollo.subgraph.name" that the latest versions of the router don't use. The correct choice is "subgraph.name".

This meant that subgraph name mapping did not work correctly in 1.45.0.

Update the mapping to reflect the change and fix subgraph name mapping for Datadog.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/5012
