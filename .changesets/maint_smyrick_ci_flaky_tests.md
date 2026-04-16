### Reduce flaky CI in federation validation and Redis cache metrics tests ([PR #9102](https://github.com/apollographql/router/pull/9102))

Removes a wall-clock performance assertion from connector validation snapshot tests (timing is unreliable under CI load) and replaces a fixed sleep in Redis cache metrics tests with polling until `command_queue_length` reports zero before asserting gauges.

By [@smyrick](https://github.com/smyrick) in https://github.com/apollographql/router/pull/9102
