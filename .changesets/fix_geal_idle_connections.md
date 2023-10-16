### Fix router hang when opening the explorer, prometheus or health check page ([Issue #3941](https://github.com/apollographql/router/issues/3941))

The Router did not gracefully shutdown when an idle connections are made by a client, and would instead hang. In particular, web browsers make such connection in anticipation of future traffic.

This is now fixed, and the Router will now gracefully shut down in a timely fashion.

---

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3969