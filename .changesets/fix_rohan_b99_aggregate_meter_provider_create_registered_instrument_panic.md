### fix: avoid panic in AggregateMeterProvider::create_registered_instrument by returning noop instrument ([PR #9248](https://github.com/apollographql/router/pull/9248))

Avoids an error message (`cannot use meter provider after shutdown`) being logged while the router is shutting down if create_registered_instrument is called by setting a noop instrument instead of panicking.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9248