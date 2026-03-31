### Apply entity-less subgraph errors to the nearest parent instead of every entity

When making an entity resolution, if entity resolution fails (for example, because the path from the subgraph was malformed), the router applied errors to every item in the list of entities expected. For example, if 2000 entities were expected but 2000 errors were returned instead, each error was applied to every entity. This causes an explosion of errors and leads to significant memory allocations that can cause OOMKills.

When the router can't determine where an error should be applied, it now applies it to the most immediate parent of the targeted entity — for a list of users, it applies to the list itself rather than to each index of that list.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/8962
