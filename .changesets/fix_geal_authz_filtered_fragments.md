### Authorization: Fix fragment filtering ([Issue #4060](https://github.com/apollographql/router/issues/4060))

If a fragment was removed, whether because its condition cannot be fulfilled or its selections were removed, then the corresponding fragment spreads must be removed from the filtered query.

This also fixes the error paths related to fragments: before, the path started at the fragment definition, while now the fragment's errors are added at the point of application

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4155