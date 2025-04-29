### Avoid unnecessary cloning in the deduplication plugin ([PR #7347](https://github.com/apollographql/router/pull/7347))

The deduplication plugin always cloned responses, even if there were not multiple simultaneous requests that would benefit from the cloned response.

We now check to see if deduplication will provide a benefit before we clone the subgraph response.

There was also an undiagnosed race condition which mean that a notification could be missed. This would have resulted in additional work being performed as the missed notification would have led to another subgraph request.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/7347
