### Increase Redis 'unresponsive' check frequency ([PR #8763](https://github.com/apollographql/router/pull/8763))

Perform the 'unresponsive' check every two seconds. This aligns with the Redis client's guideline that the check interval should be less than half the timeout value.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8763
