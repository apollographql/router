### Fix defered response formatting when filtering queries ([PR #3298](https://github.com/apollographql/router/pull/3298))

We currently use a path and subselection to identify deferred fragments and format individual responses. Unfortunately, with query filtering, matching those fragments between the original and filtered query is hard because they can have a different shape, and thus a different subselection.
As an alternative solution, we can add a label to every deferred fragment. The label argument in the `@defer` directive is optional, but unique across the entire query. Some clients use it for that exact purpose, identifying each deferred response.
So the idea here is to add labels when they are not present, use all the labels to identify the fragments, and then remove the extraneous labels from the responses sent to the client

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3298