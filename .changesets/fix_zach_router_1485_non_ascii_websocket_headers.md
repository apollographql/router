### Update tokio-tungstenite to v0.29.0 to support non-ASCII WebSocket headers ([Issue #1485](https://github.com/apollographql/router/issues/1485))

Updates `tokio-tungstenite` from v0.28.0 to v0.29.0, which includes a fix for handling non-ASCII (UTF-8) characters in WebSocket header values. Previously, WebSocket connections would fail when headers contained non-ASCII characters like "Montréal" due to serialization issues in the underlying `tungstenite` library.

This change enables the router to properly handle WebSocket connections with UTF-8 encoded header values, improving international character support for WebSocket clients.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/9051
