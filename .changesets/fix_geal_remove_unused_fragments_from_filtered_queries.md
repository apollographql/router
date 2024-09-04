### Create valid documents with authorization filtering ([PR #5952](https://github.com/apollographql/router/pull/5952))

This fixes the authorization plugin's query filtering to remove unused fragments and input arguments if the related parts of the query are removed. This was generating validation errors when the query went into query planning

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5952