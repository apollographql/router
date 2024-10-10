### Datadog priority sampling resolution is lost ([PR #6017](https://github.com/apollographql/router/pull/6017))

Previously a `x-datadog-sampling-priority` of `-1` would be converted to `0` for downstream requests and `2` would be converted to `1`.
This means that when propagating to downstream services a value of USER_REJECT would be transmitted as AUTO_REJECT.

This PR fixes this by ensuring that the `x-datadog-sampling-priority` is transmitted as is to downstream services.

