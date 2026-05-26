### Record `apollo.router.operations.coprocessor.duration` even when the coprocessor call times out ([PR #9296](https://github.com/apollographql/router/pull/9296))

`apollo.router.operations.coprocessor.duration` is now recorded even when a coprocessor call is cut short by a router timeout.  Previously, the metric was only emitted when the call completed normally, leaving timeout latencies invisible in the histogram.

By [@conwuegb](https://github.com/conwuegb) and [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9296
