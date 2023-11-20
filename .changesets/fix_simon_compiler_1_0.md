### Port to apollo-compiler 1.0 beta ([PR #4038](https://github.com/apollographql/router/pull/4038))

Version 1.0 is a near-complete rewrite of `apollo-compiler`.
Using it in the Router unblocks a lot of upcoming work.

As a more immediate benefit, some serialization-related bugs including 
[Issue #3541](https://github.com/apollographql/router/issues/3541) are fixed.
The representation of GraphQL documents in previous compiler versions was immutable.
When modifying a query (such as to remove `@authenticated` fields from an unauthenticated request)
the Router would build a new data structure with `apollo-encoder`, serialize it, and reparse it.
`apollo-compiler`` 1.0 allows mutating a document in-place, skipping the serialization step entirely.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/4038
