### do not count the wait time in deduplication as processing time ([PR #6207](https://github.com/apollographql/router/pull/6207))

waiting for a deduplicated request was incorrectly counted as time spent in the router overhead, while most of it was actually spent waiting for the subgraph response.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/6207