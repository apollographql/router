### Fix redis reconnections ([Issue #3045](https://github.com/apollographql/router/issues/3045))

The reconnection policy was using an exponential backoff delay with a maximum number of attempts. Once that maximum is reached, reconnection was never tried again (there's no baseline retry). We change that behaviour by adding infinite retries with a maximum delay of 2 seconds, and a timeout of 1 millisecond on redis commands, so that the router can continue serving requests in the meantime.

This commit contains additional fixes:
- release the lock on the in memory cache while waiting for redis, to let the in memory cache serve other requests
- add a custom serializer for `SubSelectionKey`: this type is used as key in a `HashMap`, which is converted to a JSON object, and object keys must be strings, so a specific serializer is needed instead of the derived one

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3509