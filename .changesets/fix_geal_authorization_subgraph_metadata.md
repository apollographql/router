### Extract authorization information for subgraph queries ([PR #4208](https://github.com/apollographql/router/pull/4208))

Query deduplication was already taking authorization information into account in its key, but that was for the global authorization context, ie the intersection of what the query authorization requires, and what the request token provides.
This was very coarse grained because we could have some subgraph queries with different authorization requirements, or even no authorization requirements.
This PR extracts the authorization info from subgraph queries, and uses it for deduplication. This now means that deduplicated queries can be shared more widely across different authorization contexts.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4208