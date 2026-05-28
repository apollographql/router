### Only delete coprocessor context keys from those that were sent in a given stage ([PR #9519](https://github.com/apollographql/router/pull/9519))

Addresses a race condition where context keys added by concurrent parallel subgraph stages could unintentionally be deleted.


By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9519
