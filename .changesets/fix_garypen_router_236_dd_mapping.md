### Use subgraph.name attribute not apollo.subgraph.name ([PR #5012](https://github.com/apollographql/router/pull/5012))

The Datadog exporter does some explicit mapping of attributes and was using a value "apollo.subgraph.name" that the latest versions of the router don't use. The correct choice is "subgraph.name".

Update the mapping to reflect this change.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/5012