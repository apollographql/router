### Support non-ASCII (UTF-8) WebSocket header values ([Issue #1485](https://github.com/apollographql/router/issues/1485), [PR #9051](https://github.com/apollographql/router/pull/9051))

The router can now handle WebSocket connections with UTF-8 encoded header values, including non-ASCII characters like "Montréal". Previously, such connections failed because of serialization issues in the underlying `tungstenite` library.

The fix comes from updating `tokio-tungstenite` from v0.28.0 to v0.29.0.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/9051
