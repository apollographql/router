### Fix transmitted header value for Datadog priority sampling resolution ([PR #6017](https://github.com/apollographql/router/pull/6017))

The router now transmits correct values of `x-datadog-sampling-priority` to downstream services.

Previously, an `x-datadog-sampling-priority` of `-1` was incorrectly converted to `0` for downstream requests, and `2` was incorrectly converted to `1`. When propagating to downstream services, this resulted in values of `USER_REJECT` being incorrectly transmitted as `AUTO_REJECT`.

