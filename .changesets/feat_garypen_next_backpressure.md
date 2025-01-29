### Enabling back-pressure in the request processing pipeline ([PR #6486](https://github.com/apollographql/router/pull/6486))

In Router 1.x, back-pressure was not maintained. Requests would be accepted by the router. This could cause issue for routers which were accepting high levels of traffic.

We are now improving the handling of back-pressure so that traffic shaping measures are more effective and integration with telemetry is improved. In particular, this means that telemetry events will not be lost due to traffic shaping and that traffic shaping now works more precisely. This will make the behaviour of the router more predictable.

For more details about how these improvements effect the router please refer to the [migrating from 1.x guide](reference/migration/from-router-v1.mdx).

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/6486
