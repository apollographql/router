### Update internal redis timeout ([PR #8863](https://github.com/apollographql/router/pull/8863))
Because mTLS handshakes can be slow in some environments, we’ve doubled our internal Redis timeout from 5s to 10s. This also increases the connection "unresponsive" threshold from 5s to 10s.


By [@aaronarinder](https://github.com/aaronarinder) in https://github.com/apollographql/router/pull/8863
