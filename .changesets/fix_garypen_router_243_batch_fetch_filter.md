### Filter fetches added to batch during batch creation ([PR #5034](https://github.com/apollographql/router/pull/5034))

Previously, the router didn't filter query hashes when creating batches. This could result in failed queries because the additional hashes could incorrectly make a query appear to be committed when it wasn't actually registered in a batch. 

This release fixes this issue by filtering query hashes during batch creation.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/5034