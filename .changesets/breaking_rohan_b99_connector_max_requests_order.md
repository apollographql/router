### A connector's `max_requests` limit is checked before plugins run ([PR #10300](https://github.com/apollographql/router/pull/10300))

The router now checks a connector's `max_requests` limit before coprocessors, rhai scripts and native plugins handle the request. A request over the limit is answered with the same error as before, but these plugins no longer see it, on the request or the response side. A request that a plugin stops (for example with a coprocessor `break`) still counts toward the limit.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/10300
