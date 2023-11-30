### Add missing schemas for Redis connections ([Issue #4173](https://github.com/apollographql/router/issues/4173))

Previously, support for additional schemas for the Redis client used in the Apollo Router were [added](https://github.com/apollographql/router/issues/3534). However, the router's Redis connection logic wasn't updated to process the new schema options. 

The Redis connection logic has been updated in this release.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/4174