### feat: add persisted query ID context key ([PR #8959](https://github.com/apollographql/router/pull/8959))


- Introduced a new context key for persisted query ID in the Apollo Router.
- Updated the PersistedQueryLayer to store the persisted query ID in the request context.
- Enhanced the Rhai engine to utilize the new persisted query ID context key.
- Added tests to verify the correct storage and retrieval of the persisted query ID from the context.
- Updated documentation to reflect the new context key for persisted queries.


By [@faisalwaseem](https://github.com/faisalwaseem) in https://github.com/apollographql/router/pull/8959