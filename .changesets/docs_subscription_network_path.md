### Document network-path considerations for long-lived subscription connections ([DXM-653](https://apollographql.atlassian.net/browse/DXM-653))

A new "Network path considerations for client connections" section in the subscription configuration docs explains how proxies, CDNs, gateways, and corporate firewalls between the client and the router can produce 504 errors. It names three failure modes (response buffering, short idle/read/response timeouts, asymmetric paths between the router and the client), points readers at the `apollo.router.operations.subscriptions.terminated.client` metric for distinguishing router-side from intermediary-side failures, and gives the configuration recommendation up front.

By [@andywgarcia](https://github.com/andywgarcia) in https://github.com/apollographql/router/pull/9318
