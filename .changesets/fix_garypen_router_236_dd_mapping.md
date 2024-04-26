### Fix subgraph name mapping of Datadog exporter ([PR #5012](https://github.com/apollographql/router/pull/5012))

Previously in the router v1.45.0, subgraph name mapping didn't work correctly in the router's Datadog exporter. The exporter used the incorrect value `apollo.subgraph.name` for mapping attributes when it should have used the value `subgraph.name`. This issue has been fixed in this release.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/5012
