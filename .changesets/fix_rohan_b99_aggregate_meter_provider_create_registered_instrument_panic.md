### fix: avoid panic in AggregateMeterProvider::create_registered_instrument by returning noop instrument ([PR #9248](https://github.com/apollographql/router/pull/9248))

Prevent spurious `cannot use meter provider after shutdown` error message during router shutdown.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9248