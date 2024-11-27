### Overhead processing metrics should exclude subgraph response time when deduplication is enabled ([PR #6207](https://github.com/apollographql/router/pull/6207))

The router's calculated overhead processing time has been fixed, where the time spent waiting for the subgraph response of a deduplicated request had been incorrectly included.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/6207