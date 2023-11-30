### Improved query deduplication with extracted authorization information from subgraph queries ([PR #4208](https://github.com/apollographql/router/pull/4208))

Query deduplication has been improved with authorization information extracted from subgraph queries. 

Previously, query deduplication was already taking authorization information into account in its key, but that was for the global authorization context (the intersection of what the query authorization requires and what the request token provides).
This was very coarse grained, leading to some subgraph queries with different authorization requirements or even no authorization requirements.

In this release, the authorization information from subgraph queries is used for deduplication. This now means that deduplicated queries can be shared more widely across different authorization contexts.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4208