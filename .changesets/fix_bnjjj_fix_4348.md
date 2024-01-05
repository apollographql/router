### Abstract spreads in queried objects no longer returning erroneous nulls ([Issue #4348](https://github.com/apollographql/router/issues/4348))

When you had an inline fragment on an union type in a fragment spread the response returned to the client was not fully accurate against the schema, or the GraphQL specification, which resulted in client errors and unexpected `null` values (either on fields or objects whose members existed on other concrete types of the union).  This is now resolved and accounted for correctly, and additional tests have been added.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4401