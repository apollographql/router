### Add persisted query ID context key ([PR #8959](https://github.com/apollographql/router/pull/8959))

Adds a context key for the persisted query ID in the router. The `PersistedQueryLayer` now stores the persisted query ID in the request context, and the Rhai engine can access it via that key.

By [@faisalwaseem](https://github.com/faisalwaseem) in https://github.com/apollographql/router/pull/8959
