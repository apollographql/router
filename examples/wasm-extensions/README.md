# WebAssembly extensions

This example runs six WebAssembly components authored in Java, Node.js, Python, Rust, Go, and Scala. Node.js, Python, Go, Java, and Scala run at `supergraph.request`; Rust runs at `subgraph.request` with a service selector. Each component adds a distinct request header and context entry. Header propagation lets the example subgraph confirm that all six components executed.

The shared WIT contract is in `apollo-router/wit/router-plugin/world.wit`.

All six components are built against the same contract, which also supports `connector.request`. Connector events identify the originating subgraph with `service-name`, the optional connector source with `source-name`, the specific `@connect` directive with `connector-name`, and the transport as `http` or `mapping_only`. HTTP connector events expose the raw transport request through the existing method, URI, header, and body fields; mapping-only events have no HTTP request fields. The Rust component also runs at `connector.request` for the local `connectors.local` source and `localMessage` connector. Its header mutation is returned by the local connector endpoint, proving the connector layer executed.

### Language support paths

| Language | Component path | Notes |
| --- | --- | --- |
| Rust | Native `wasm32-wasip2` | `wit-bindgen` generates bindings at compile time. |
| Go | Native Go WASI module plus Preview 1 adapter | The maintained `wit-bindgen go` generator emits the Canonical ABI bindings. |
| Node.js | ComponentizeJS | Jco embeds the JavaScript implementation in a WASI component. |
| Python | `componentize-py` | CPython and generated WIT bindings are embedded in the component. |
| Java | TeaVM Java-to-JavaScript plus ComponentizeJS | Current `wit-bindgen` no longer has a maintained Java generator, so a small JavaScript WIT adapter calls the Java-authored logic. |
| Scala | Scala.js plus ComponentizeJS | Scala has no direct WIT generator; a small JavaScript WIT adapter calls the Scala-authored logic. |

Java and Scala are intentionally described as transpiled paths rather than native WIT paths. Their language code executes inside the component, but the Canonical ABI boundary is supplied by ComponentizeJS.

## Configuration model

The `wasm` configuration deliberately has no schema-version field. Each plugin has a stable identity, a tagged `source`, opaque plugin-owned `configuration`, hook declarations, and optional limit and failure overrides. A digest is part of its source because it verifies the exact bytes resolved from that source:

```yaml
wasm:
  plugins:
    - name: policy
      source:
        type: file
        path: ./policy.wasm
        digest: sha256:...
      configuration:
        policy_name: checkout
      hooks:
        - hook: subgraph.request
          selector:
            service_names: [products]
          permissions:
            headers:
              read:
                names: [authorization]
              write:
                names: [x-policy-result]
```

Unknown host-owned fields are rejected so mistakes fail at startup. The value under `configuration` is owned by the component and can evolve without changing the Router schema. Permissions default to no access, while selectors and tagged source variants leave room for additional hooks, source transports, signature metadata, and capabilities later.

Connector hooks use a source selector independently of the originating subgraph:

```yaml
hooks:
  - hook: connector.request
    selector:
      service_names: [products]
      source_names: [inventory]
      connector_names: [inventory-by-id]
    permissions:
      transport:
        method: read
        uri: read_write
        body: read_write
      headers:
        read:
          names: [authorization]
        write:
          names: [x-policy-result]
```

## Build and verify all guests

Install the prerequisites listed below, then use the shared entry points from the repository root:

```sh
examples/wasm-extensions/build-all.sh
examples/wasm-extensions/verify.sh
```

The build script produces all six components and the Router binary. The verification script validates each component and its WIT export, starts the example subgraph and Router, and checks the end-to-end response.

## Build individual guests

Rust requires the `wasm32-wasip2` target:

```sh
rustup target add wasm32-wasip2
cd examples/wasm-extensions/rust-header
cargo build --release --target wasm32-wasip2
```

Node.js uses Jco and ComponentizeJS. Dependencies are installed once at the example root and shared by the Node.js, Java, and Scala builds:

```sh
cd examples/wasm-extensions
npm install
npm run build:node
```

Python uses `componentize-py`:

```sh
cd examples/wasm-extensions/python-header
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
.venv/bin/componentize-py -d ../../../apollo-router/wit/router-plugin -w router-plugin bindings .
.venv/bin/componentize-py -d ../../../apollo-router/wit/router-plugin -w router-plugin componentize plugin -o plugin.wasm --stub-wasi
```

Go requires Go 1.25 or newer, `wit-bindgen` 0.60 or newer, and `wasm-tools`. The build script generates bindings, builds a WASI module, downloads the pinned Preview 1 reactor adapter, and creates a component:

```sh
cargo install --locked wit-bindgen-cli --version 0.60.0
cargo install --locked wasm-tools
cd examples/wasm-extensions/go-header
./build.sh
```

Java requires Java 17 or newer, Maven, and the Node.js tools installed above. TeaVM compiles the Java method to an ES module before the wrapper is componentized:

```sh
cd examples/wasm-extensions/java-header
./build.sh
```

Scala requires Scala CLI and the Node.js tools installed above. Scala.js compiles the exported Scala method to an ES module before the wrapper is componentized:

```sh
cd examples/wasm-extensions/scala-header
./build.sh
```

## Run the example manually

From the repository root, start the subgraph and Router in separate terminals:

```sh
python3 examples/wasm-extensions/subgraph.py
cargo run -p apollo-router -- \
  --config examples/wasm-extensions/router.yaml \
  --supergraph examples/wasm-extensions/supergraph.graphql
```

Then query the Router:

```sh
curl http://127.0.0.1:4100/ \
  -H 'content-type: application/json' \
  --data '{"query":"{ me }"}'
```

The response should be:

```json
{"data":{"me":"active,active,active,active,active,active","connectorMessage":{"value":"active"}}}
```
