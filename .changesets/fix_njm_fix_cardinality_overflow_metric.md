### Fix OTel cardinality overflow metric for more error strings ([PR #8740](https://github.com/apollographql/router/pull/8740))

Emit the apollo.router.telemetry.metrics.cardinality_overflow metric for more instances where an OTel cardinality error has occurred. The message check has been changed to support a different form of the error that has been reported by a customer.

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/8740
