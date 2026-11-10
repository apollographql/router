### Draw request ids without a system call, and skip empty trace-context injection and per-frame fetch counts

The router read every request id and subgraph request id from the operating system, one system call per id. It now draws them from a thread-local cryptographic generator that the operating system seeds, and the ids are still random version 4 UUIDs.

Subgraph and connector requests no longer build an OpenTelemetry context for trace propagation when no propagator is configured: the router injects trace context only through a propagator that names the headers it writes, and every propagator the router configures does.

The `apollo.router.operations.fetch.response_size` counter is now incremented once per subgraph response, with the response's total bytes, instead of once per response frame. Its totals are unchanged.

By [@martijnwalraven](https://github.com/martijnwalraven)
