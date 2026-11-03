### Connectors can call MCP servers that require sessions

Some Model Context Protocol servers, especially ones built with an SDK's stateful defaults, reject a `tools/call` that does not carry the `Mcp-Session-Id` they issued from an earlier `initialize`. A connector source can now ask the router to hold those sessions:

```yaml
connectors:
  sources:
    my_subgraph.my_mcp_source:
      mcp:
        sessions: auto
```

With `sessions: auto`, the router sends `initialize` before a caller's first request to the source, then sends every request from that caller within the session, with the `Mcp-Session-Id` and negotiated `MCP-Protocol-Version` headers and a JSON-RPC `id` unique within the session. Requests that arrive together wait on one handshake. When the server answers `404` because it has forgotten the session, the router starts a new one and sends the request again, once. A server that issues no session id, or negotiates protocol revision 2026-07-28 or later, is remembered as stateless and receives requests exactly as the connector wrote them.

Callers never share a session. The router keys sessions by the `iss`, `sub` and `sid` claims of a JWT validated by its authentication plugin, so a login keeps its session across token refreshes, or otherwise by the `Authorization`, `Cookie` and `Proxy-Authorization` headers sent to the source. `identity_headers` lists more headers to split sessions by. `max_sessions` (default 50) and `idle_timeout` (default 30m) bound how many sessions are held; a session dropped to make room or for idling is closed with `DELETE`.

Sessions are off by default. The router reports adoption with the `opt.source.mcp_sessions` attribute of the `apollo.router.config.connectors` gauge.

By [@benjamn](https://github.com/benjamn)
