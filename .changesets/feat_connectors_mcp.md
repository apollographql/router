### Connectors can call MCP tools with `@source(mcp:)` and `@connect(mcp:)` (connect/v0.5)

A connector can now call a tool on a Model Context Protocol server without writing the protocol by hand. The source says it is an MCP server, and each connector names a tool and builds its arguments:

```graphql
extend schema
  @link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@connect", "@source"])
  @source(name: "quakes", http: { baseURL: "https://quakes.example.com" }, mcp: { path: "/mcp" })

type Query {
  recentQuakes(limit: Int): [Quake]
    @connect(
      source: "quakes"
      mcp: { tool: "search_events", arguments: "$args { limit }" }
      selection: "$.events { id magnitude }"
    )
}
```

The router sends the JSON-RPC `tools/call` request to the source's MCP endpoint, which is `http.baseURL` followed by `mcp.path`. It reads the reply whether the server answers with JSON or with Server-Sent Events, skipping progress notifications, and stops reading an event stream at the reply, so a server that leaves the stream open doesn't hold up the field. The selection receives the tool's output: its `structuredContent`, or else the text of its text blocks as a string. Text is never parsed implicitly, so a tool's output keeps one type from call to call; map a tool that writes JSON as text with `$->jsonParse`. `arguments` is a mapping like `http.body`, so it can use `$args`, `$this`, `$batch` and the other request variables. A tool without arguments is called with `{}`.

MCP reports a failure in one of two ways, both with HTTP 200: a JSON-RPC `error`, or a tool result with `isError: true`. Without `isSuccess` or `errors` mappings, a connector reports each as a GraphQL error with its own code:

- `MCP_PROTOCOL_ERROR` for a JSON-RPC `error`, such as an unknown tool or invalid arguments, with the JSON-RPC error code in `extensions.mcp.code`. The error's `data` is not passed on.
- `MCP_TOOL_ERROR` for a tool result with `isError: true`, with the tool's text as the message.
- `MCP_INVALID_RESPONSE` when a successful HTTP response holds no JSON-RPC reply, or a reply whose `result` isn't a tool result.
- `MCP_UNSUPPORTED_CONTENT` for a tool result with neither `structuredContent` nor text, such as one holding only images, audio or resources, which `@connect(mcp:)` doesn't map.
- `UNAUTHENTICATED` for a 401 and `FORBIDDEN` for a 403. When the server sends a `Bearer` challenge in `WWW-Authenticate`, its `resource_metadata`, `scope`, `error` and `error_description` are passed on in `extensions.mcp.auth` (as `resourceMetadata`, `scope`, `error` and `errorDescription`), so a client that can get a token from the server's authorization server knows where to start, and can retry with it.

A connector's own `isSuccess` and `errors` mappings, including ones it inherits from its `@source`, see the JSON-RPC reply instead of the tool's output.

A source with `@source(mcp:)` holds MCP sessions without any router configuration, for servers that require them. `connectors.sources.<subgraph>.<source>.mcp.sessions: off` turns that off.

`mcp` is only defined from connect/v0.5, which is still in preview. Schemas linking earlier versions are unchanged. Validation reports `INVALID_MCP` when `mcp:` is used before v0.5, combined with `http:` on the same connector, used without a source that declares `mcp:`, or given an empty tool name. The router reports adoption with the `apollo.router.schema.connectors.mcp` gauge, which counts the connectors that call MCP tools, and counts calls with `apollo.router.connectors.mcp.calls`, by `connector.source`, `mcp.tool.name` and `mcp.outcome` (`ok`, `tool_error`, `protocol_error`, `invalid_response`, `unsupported_content`, `auth_error` or `error`), since tool and protocol errors arrive with HTTP 200 and HTTP metrics count them as successes.

This release speaks the MCP revisions that begin with an `initialize` handshake, up to 2025-11-25. A server that only accepts the 2026-07-28 request form, which drops `initialize` and requires per-request `Mcp-Method` and `Mcp-Name` headers and metadata, isn't supported yet.

By [@benjamn](https://github.com/benjamn)
