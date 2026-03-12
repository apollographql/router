### Don't apply entity-less errors from subgraphs greedily

When making an entity resolution, if for some reason we failed to get an entity (eg, the path was malformed from the subgraph), we'd apply any errors to _everything_ in the list of entities we expected. So, for example, if we were to get 2000 entities and instead received 2000 errors, we'd apply each error to every entity we expected to exist. That causes an explosion of errors and leads to significant memory allocations that almost certainly lead to OOMKills.

Now, when we don't know where an error should be applied, we apply it to the most immediate parent of the targeted entity (so in the case of a list of users, it'd apply to the list itself rather than to each index of that list).

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/8962
