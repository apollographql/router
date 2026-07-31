# WebAssembly extensions

This example runs three WebAssembly components authored in Rust, JavaScript, and Python. JavaScript and Python run at `supergraph.request`; Rust runs at `subgraph.request` with a service selector. Each component adds a distinct request header and context entry. Header propagation lets the example subgraph confirm that all three components executed.

The shared WIT contract is in `apollo-router/wit/router-plugin/world.wit`.

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

## Build the guests

Rust requires the `wasm32-wasip2` target:

```sh
rustup target add wasm32-wasip2
cd examples/wasm-extensions/rust-header
cargo build --release --target wasm32-wasip2
```

JavaScript uses `jco` and ComponentizeJS:

```sh
cd examples/wasm-extensions/javascript-header
npm install
npm run build
```

Python uses `componentize-py`:

```sh
cd examples/wasm-extensions/python-header
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
.venv/bin/componentize-py -d ../../../apollo-router/wit/router-plugin -w router-plugin bindings .
.venv/bin/componentize-py -d ../../../apollo-router/wit/router-plugin -w router-plugin componentize plugin -o plugin.wasm --stub-wasi
```

## Run the example

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
{"data":{"me":"active,active,active"}}
```
