### Fix router hang when opening the explorer, prometheus or health check page ([Issue #3941](https://github.com/apollographql/router/issues/3941))

The router's graceful shutdown was not handling idle connections well, and waiting indefinitely for them to close instead of shutting them down directly, which resulted in the router hanging on CTRL+C.

This tends to happen in particular when a browser opens the explorer or prometheus page provided by the router, because browsers eagerly open new connectionsi in anticipation of future traffic, while those pages only need a single request to the router.

---

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3969