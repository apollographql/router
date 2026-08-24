### Fix a race that could leave subscription reconnect/forwarding tasks hung after all clients disconnect

When every client of a router-side subgraph subscription disconnected, the pubsub notification system was supposed to detect that no receivers were left and send a "closing" signal to tear down the subscription's forwarding task. Because of struct field drop order, the "a client unsubscribed" notification could be sent to the pubsub task *before* the broadcast receiver it depended on was actually dropped and its count decremented. Under real concurrent scheduling, the pubsub task could process that notification while still seeing a live receiver, conclude a subscriber was still present, and never send the closing signal — leaving the forwarding task hung indefinitely (e.g. sleeping out a reconnect delay, or stuck on a subgraph's reconnect handshake) instead of shutting down.

`Handle` and `HandleStream` in the subscription notification module now drop their broadcast receiver before the guard that sends the unsubscribe notification, closing the race.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/PULL_NUMBER
