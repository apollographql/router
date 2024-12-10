
### Remove catch_unwind wrapper around the native query planner ([PR #6397](https://github.com/apollographql/router/pull/6397))

As part of internal maintenance of the query planner, we are removing the
catch_unwind wrapper around the native query planner. This wrapper was used as
an extra safeguard for potential panics the native planner could produce. The
native query planner no longer has any code paths that could panic. We have also
not witnessed a panic in the last four months, having processed 560 million real
user operations through the native planner. 

This maintenance work also removes backtrace capture for federation errors which
was used for debugging and is no longer necessary as we have the confidence in
the native planner's implementation.

By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/6397
