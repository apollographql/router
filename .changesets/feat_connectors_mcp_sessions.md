### Connectors can call MCP servers that require sessions

Some Model Context Protocol servers, especially ones built with an SDK's stateful defaults, reject a `tools/call` that does not carry the `Mcp-Session-Id` they issued from an earlier `initialize`. A connector source can now ask the router to hold those sessions:

```yaml
connectors:
  sources:
    my_subgraph.my_mcp_source:
      mcp:
        sessions: auto
```

With `sessions: auto`, the router sends `initialize` before a caller's first request to the source, then sends every request from that caller within the session, with the `Mcp-Session-Id` and negotiated `MCP-Protocol-Version` headers and a JSON-RPC `id` unique within the session. Requests that arrive together wait on one handshake. When the server answers `404` because it has forgotten the session, the router starts a new one and sends the request again, once. It does the same when the server answers `401` or `403` to a request whose credentials differ from the ones the session was opened with, as servers that tie sessions to tokens do after a token refresh, and closes the old session. A server that issues no session id, or negotiates protocol revision 2026-07-28 or later, is remembered as stateless and receives requests exactly as the connector wrote them.

Callers never share a session. The router keys sessions by the `iss`, `sub` and `sid` claims of a JWT validated by its authentication plugin, so a login keeps its session across token refreshes, or otherwise by the `Authorization`, `Cookie` and `Proxy-Authorization` headers sent to the source. `identity_headers` lists more headers to split sessions by. `max_sessions` (default 50) and `idle_timeout` (default 30m) bound how many sessions are held; a session dropped to make room or for idling is closed with `DELETE`, and so is every open session when a reload or shutdown replaces the router's pipeline. Requests keep the extensions their connector request had, so a SigV4 source signs the handshake and `DELETE` as well.

Sessions are off by default, except for a source that declares `@source(mcp:)`, which holds them unless its configuration sets `sessions: off`. The router reports adoption with the `opt.source.mcp_sessions` attribute of the `apollo.router.config.connectors` gauge, and counts handshakes with `apollo.router.connectors.mcp.handshakes`, by `connector.source`, `mcp.handshake.reason` (`new`, `expired` or `credentials_changed`) and `mcp.handshake.outcome` (`session`, `stateless`, `refused` or `failed`). Handshakes don't go through the HTTP client's own instruments.

By [@benjamn](https://github.com/benjamn)
