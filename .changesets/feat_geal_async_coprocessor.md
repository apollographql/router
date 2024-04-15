### asynchronous coprocessor execution ([Issue #3297](https://github.com/apollographql/router/issues/3297))

This adds a new `asynchronous` option at all coprocessor stages to allow the router to continue handling the request or response without waiting for the coprocessor to respond. This implies that the coprocessor response will not be used. This is targeted at use cases like logging and auditing, which do not need to block the router

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4902