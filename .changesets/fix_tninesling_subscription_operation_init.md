### Eagerly init subgraph operation for subscription primary nodes ([PR #6509](https://github.com/apollographql/router/pull/6509))

When subgraph operations are deserialized, typically from a query plan cache, they are not automatically parsed into a full document. Instead, each node needs to initialize its operation(s) prior to execution. With this change, the primary node inside SubscriptionNode is initialized in the same way as other nodes in the plan.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/6509
