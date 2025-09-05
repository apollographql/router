### Fix _entities Apollo Error Metrics Missing Service Attribute ([PR #8153](https://github.com/apollographql/router/pull/8153))

Error counting https://github.com/apollographql/router/pull/7712 introduced a bug where `_entities` errors from a subgraph fetch no longer reported a service (subgraph or connector) attribute. This erroneously categorized these errors as from the Router rather than their originating service in the Studio UI.

The attribute has been re-added, fixing this issue.

By [@rregitsky](https://github.com/rregitsky) in https://github.com/apollographql/router/pull/8153
