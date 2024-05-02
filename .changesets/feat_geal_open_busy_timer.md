### Make the busy timer public ([PR #4989](https://github.com/apollographql/router/pull/4989))

the busy timer is used to measure the time spent working on a request outside of waiting for external calls like coprocessors and subgraph calls. It still includes the time spent waiting for other concurrent requests to be handled (wait time in the executor) to show the actual router overhead in the request handling.
This makes the busy timer API public to let native plugins use it when they do their own network calls. The affected methods are `Context::enter_active_request` and `Context::busy_time`.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4989