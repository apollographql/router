### Fix fragment spread response formatting with aliased typename ([Issue #4348](https://github.com/apollographql/router/issues/4348))

When you had an aliased `__typename` (example `myAlias: __typename`) in a fragment spread, the response formatting was wrong.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4401