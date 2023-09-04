### Subscriptions: Correct v1.28.x regression allowing panic via un-named subscription operation

Correct a regression that was introduced in Router v1.28.0 which made a Router **panic** possible when the following _three_ conditions are _all_ met:

1. When sending an un-named (i.e., "anonymous") `subscription` operation (e.g., `subscription { ... }`); **and**;
2. The Router has a `subscription` type defined in the Supergraph schema; **and**
3. Have subscriptions enabled (they are disabled by default) in the Router's YAML configuration, either by setting `enabled: true` _or_ by setting a `mode` within the `subscriptions` object (as seen in [the subscriptions documentation](https://www.apollographql.com/docs/router/executing-operations/subscription-support/#router-setup).

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/3738
