### Filter the fetches we add to a batch when we create the batch ([PR #5034](https://github.com/apollographql/router/pull/5034))

Without filtering the query hashes during batch creation we could end up in a situation where we have additional query hashes in the batch.

This manifests itself by batch queries failing with query hashes appearing as committed without ever having been registered in a batch.

Filtering during batch creation is now matching the filtering at subgraph coordination and fixes this issue.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/5034