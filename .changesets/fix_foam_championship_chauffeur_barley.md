### Populate per-type metrics based on FTV1 from subgraphs ([Issue #2551](https://github.com/apollographql/router/issues/2551))

[Since version 1.7.0](https://github.com/apollographql/router/blob/dev/CHANGELOG.md#traces-wont-cause-missing-field-stats-issue-2267)Apollo Router generates metrics directly, instead of having them derived from tracing by Apollo Studio. However these metrics were incomplete. This adds, based on data reported by subgraphs:

* Statistics about each field of each type of the GraphQL type system
* Statistics about errors at each path location of GraphQL responses

Fixes https://github.com/apollographql/router/issues/2551
Closes https://github.com/mdg-private/planning/issues/1814

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/2541
