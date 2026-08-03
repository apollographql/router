### Add preview support for sandboxed WebAssembly plugins ([PR #9920](https://github.com/apollographql/router/pull/9920))

Apollo Router can now run WebAssembly components at `supergraph.request`, `subgraph.request`, and `connector.request` hooks. Components implement the versioned `apollo:router-plugin@0.1.0` WIT contract and return declarative request mutations or, where supported, early GraphQL responses.

Configure components under the new top-level `wasm` key. Per-hook permissions restrict access to headers, context, GraphQL bodies, and Connector transport fields. Per-plugin limits bound execution time, queueing, concurrency, linear memory, and input/output payloads. Components run without inherited environment, filesystem, standard I/O, TCP, or UDP access. Local file sources can include a SHA-256 digest alongside the path for startup integrity verification.

The repository includes build and end-to-end verification examples for Rust, Go, Node.js, Python, Java, and Scala components.
