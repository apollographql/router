### Add environment variable for MAX_NON_LOCAL_SELECTIONS 

Adds a environment variable for MAX_NON_LOCAL_SELECTIONS const used in query
planning traversal. This is an undocumented env var, as it's intended for
internal use. 

This additional adds a metric for tracking current number of non-local
selections, which is a histogram available under
`apollo.router.query_planning.plan.non_local_selections`.

By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/10349
