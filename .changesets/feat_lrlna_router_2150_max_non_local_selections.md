### Add a metric to track non-local selection sets observed during query planning 


`apollo.router.query_planning.plan.non_local_selections` is a histogram of the
number of non-local selections estimated during query planning traversal used to
limit the number of options explored. The non-local selections limit defaults to
100_000. Numbers observed to be close to this limit may warrant an investigation
into complexity of the operations.

By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/10349
