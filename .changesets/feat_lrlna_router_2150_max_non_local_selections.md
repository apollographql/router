### Add a metric to track non-local selection sets observed during query planning 

`apollo.router.query_planning.plan.non_local_selections` is a histogram of the
number of non-local selections estimated during query planning traversal. An
investigation into the complexity of operations is warranted should this metric
return values close to the internal non-local selections limit of 100000. The
histogram uses fixed bucket boundaries of 100, 1000, 25000, 50000, 100000, and
150000.

By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/10349
