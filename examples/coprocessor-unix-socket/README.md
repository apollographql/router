# Coprocessor Unix Domain Socket Example

This example demonstrates how to configure the Apollo Router to communicate with a coprocessor using Unix Domain Sockets (UDS) instead of HTTP. UDS provides lower latency and reduced overhead compared to HTTP networking.

## Benefits of Unix Domain Sockets

- **Lower Latency**: Eliminates TCP/IP stack overhead for local communication
- **Reduced Resource Usage**: No network layer processing required
- **Enhanced Security**: Communication occurs through filesystem permissions
- **Better Performance**: Direct kernel-level IPC mechanism

## Configuration

Configure your router to use a Unix socket URL:

```yaml
coprocessor:
  url: unix:///tmp/coprocessor.sock
  router:
    request:
      headers: true
      context: true
  supergraph:
    response:
      body: true
```

## URL Format

The router supports three URL schemes for coprocessors:

- **HTTP**: `http://localhost:8080/coprocessor`
- **HTTPS**: `https://coprocessor.example.com:8443/webhook`
- **Unix Domain Socket**: `unix:///path/to/socket.sock`

## Coprocessor Implementation

Your coprocessor must listen on the specified Unix socket path. Here's a Node.js example:

```javascript
const http = require('http');
const fs = require('fs');

const SOCKET_PATH = '/tmp/coprocessor.sock';

// Remove existing socket file if it exists
if (fs.existsSync(SOCKET_PATH)) {
  fs.unlinkSync(SOCKET_PATH);
}

const server = http.createServer((req, res) => {
  // Handle coprocessor requests
  res.writeHead(200, { 'Content-Type': 'application/json' });
  res.end(JSON.stringify({ version: 1, stage: req.body?.stage }));
});

server.listen(SOCKET_PATH, () => {
  console.log(`Coprocessor listening on unix socket: ${SOCKET_PATH}`);

  // Set appropriate permissions
  fs.chmodSync(SOCKET_PATH, 0o666);
});
```

## Migration from HTTP

To migrate from HTTP to Unix sockets:

1. Update your coprocessor to listen on a Unix socket
2. Change the router configuration URL from `http://...` to `unix://...`
3. Restart both the coprocessor and router

The configuration is fully backward compatible - existing HTTP configurations continue to work unchanged.

## Observability

Unix socket connections are traced with appropriate transport-specific attributes:

- `net.transport`: Set to "unix" for Unix sockets, "ip_tcp" for HTTP
- `server.address`: Shows the socket path for UDS connections
- `url.full`: Displays the original `unix://` URL for better observability

## Security Considerations

- Set appropriate filesystem permissions on the socket file
- Ensure the socket directory is accessible to both router and coprocessor processes
- Consider using dedicated directories with restricted access (e.g., `/var/run/apollo/`)