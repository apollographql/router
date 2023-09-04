### remove clones from the header plugin ([Issue #3068](https://github.com/apollographql/router/issues/3068))

The list of header operations was cloned for every subgraph query, and this was increasing latency. We made sure the overhead is minimal by removing those allocations

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3721