### Return null data and respect error location config for fully-unauthorized requests ([PR #9022](https://github.com/apollographql/router/pull/9022))

When the query planner rejected a request because all fields were unauthorized, the response always placed errors in the `errors` array and returned `data: {}`, ignoring the configured `errors.response` location (`errors`, `extensions`, or `disabled`). Router now returns `data: null` and respects `errors.response` and `errors.log`, consistent with partially-unauthorized requests.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9022
