### Increase Redis 'unresponsive' check frequency ([PR #8763](https://github.com/apollographql/router/pull/8763))

Perform the 'unresponsive' check every two seconds; this change aligns us with the Redis client's guideline of the check
interval being less than half the timeout value.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8763
