### Increase internal Redis timeout from 5s to 10s ([PR #8863](https://github.com/apollographql/router/pull/8863))

Because mTLS handshakes can be slow in some environments, the internal Redis timeout is now 10s (previously 5s). The connection "unresponsive" threshold is also increased from 5s to 10s.

By [@aaronarinder](https://github.com/aaronarinder) in https://github.com/apollographql/router/pull/8863
