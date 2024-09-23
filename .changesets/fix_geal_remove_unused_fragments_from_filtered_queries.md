### Create valid documents with authorization filtering ([PR #5952](https://github.com/apollographql/router/pull/5952))

This release fixes the authorization plugin's query filtering to remove unused fragments and input arguments if the related parts of the query are removed. Previously the plugin's query filtering generated validation errors when planning the query.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5952