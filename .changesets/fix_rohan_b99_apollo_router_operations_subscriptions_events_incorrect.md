### Correct `apollo.router.operations.subscriptions.events` metric counting ([PR #8483](https://github.com/apollographql/router/pull/8483))

The `apollo.router.operations.subscriptions.events` metric now increments correctly for each subscription event (excluding ping/pong/close messages). The counter call has been moved into the stream to trigger on each event.

This change also removes custom pong response handling before connection acknowledgment, which previously caused duplicate pongs because the WebSocket implementation already handles pings by default.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8483