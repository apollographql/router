### fix(subscription): force the deduplication to be enabled by default as it's documented ([PR #3773](https://github.com/apollographql/router/pull/3773))

A bug was introduced in router v1.25.0 which caused [subscription deduplication](https://www.apollographql.com/docs/router/executing-operations/subscription-support#subscription-deduplication) to be disabled by default.
As documented, the router will enable deduplication by default, providing you with subscriptions that scale.

Should you decide to disable it, you can still explicitly set `enable_deduplication` to `false`.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3773
