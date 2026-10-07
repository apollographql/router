### Fix a subscription cleanup race that could leave stale subgraph connections and background tasks running

When every client of a router-side subscription disconnected, a race condition could occasionally prevent the router from noticing, so it never tore down the corresponding subgraph connection. The subgraph-facing task would keep running in the background — retrying its reconnect delay or handshake — even though no client was left to receive events, until some other event eventually cleaned it up.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/10075
