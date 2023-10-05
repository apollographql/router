### Fix hang and high CPU usage when compressing small responses ([PR #3961](https://github.com/apollographql/router/pull/3961))

When returning small responses (less than 10 bytes) and compressing them using gzip, the router could go into an infinite loop

---

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3961