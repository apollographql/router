### Fix `apollo.router.operations.subscriptions.events` metric not incrementing for subscription events ([PR #8483](https://github.com/apollographql/router/pull/8483))

Moves the `u64_counter!` call into the stream so it triggers for each subscription event (ignoring ping/pong/close etc).
Also removes the custom sending of pong responses before connection ack is received, which resulted in 2 pongs being sent as the websocket implementation already replies to pings by default.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8483