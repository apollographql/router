### remove clones from the header plugin ([Issue #3068](https://github.com/apollographql/router/issues/3068))

The list of header operations was cloned for every subgraph query, and this was increasing latency. By storing them in an reference counted structure, we make sure the overhead is minimal.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3721