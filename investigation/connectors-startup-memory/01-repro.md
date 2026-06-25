# 01 — Reproduction (parametric connectors vs pure-subgraph supergraphs)

> Phase 1. Read `00-context.md` first. This phase builds a **parametric, validated**
> reproduction. No measurement yet (that's Phase 2). Everything here is committed on
> branch `smyrick/6817694b`.

## What was built

`scripts/gen_schema.py` — generates two comparable supergraph **families** at parameter `N`
(number of source subgraphs), with `K` plain connector/query fields and `E` shared `@key`
entity types per subgraph:

- **connectors**: each subgraph declares one `@source` and `K + E` `@connect` directives
  (GET + JSONSelection); the `E` shared entities use `entity: true` resolver connectors.
  The router expands **every `@connect` into its own synthetic subgraph** at startup, so
  `synthetic_subgraphs = N × (K + E)`.
- **pure**: identical type/field counts and the same shared `@key` entities, but plain
  GraphQL (no `@source`/`@connect`). `subgraphs = N`. This is the **control** that isolates
  connectors overhead (directly answers Renée's "is this connectors-specific?" question).

Both families define the same `E` `SharedJ @key(fields: "id")` entity types in **every**
subgraph (each contributes distinct fields `s{i}_a/s{i}_b`), forcing the cross-subgraph
entity-resolution path (`build_query_graph::handle_key`, ~`O(S²)`).

Query field names are subgraph-unique (`svc{i}_item{j}`, `svc{i}_shared{j}`) to avoid
`@shareable` composition conflicts; only the entity **types** are shared across subgraphs.

## Syntax provenance (grounded in this worktree, not hand-rolled)

The plan wanted the apollo-connectors skill reference files fetched into
`scripts/connectors-skill-refs/`. That external fetch was blocked by auto-review (network
transfer). Instead the generator is grounded in **this repo's own validated connector
fixtures**, which are stronger truth than external docs for this worktree
(`apollo-federation v2.15.0`, `connect/v0.3` = `ConnectSpec::latest()`):

- connector subgraph SDL + `supergraph.yaml` shape:
  `apollo-federation/src/connectors/tests/schemas/simple.{yaml,graphql}`
- `entity: true` resolver connector:
  `apollo-federation/src/connectors/validation/test_data/keys_and_entities/valid/basic_implicit_key.graphql`
- versions used: `federation/v2.12` + `connect/v0.3`; rover `federation_version: =2.12.0`.

## Key tooling gotcha (encoded in the generator)

`gen_schema.py` writes each subgraph to its own `svc{i}.graphql` and references it via
`file:` in `supergraph.yaml` — **not** inline `sdl:`. Reason: `rover supergraph compose`
runs `${...}` env-style variable expansion over the config YAML and corrupts connect
templates like `{$args.id}` (`error: Invalid variable expansion key: args`). Schema
**file** contents are read raw, sidestepping the expansion.

## Exact commands

```bash
cd investigation/connectors-startup-memory

# one family, one N (writes svc*.graphql + supergraph.yaml into the dir):
python3 scripts/gen_schema.py --family connectors --n 8 --k 8 --e 4 --out-dir artifacts/connectors_N8

# compose (customer path — leaves @connect for the router to expand at startup):
APOLLO_ELV2_LICENSE=accept rover supergraph compose \
  --config artifacts/connectors_N8/supergraph.yaml > artifacts/connectors_N8/supergraph.graphql

# full sweep, both families (N in {1,2,4,8,16,32,64}, K=8, E=4):
bash scripts/compose_all.sh          # writes artifacts/<family>_N<n>/ + artifacts/manifest.tsv
```

Fallback compose (in-repo, no network, same `compose_with_connectors` the router ships):
`cargo run -p apollo-federation-cli -- compose --config <dir>/supergraph.yaml`.
**Caveat**: the federation-cli `compose` **pre-expands** connectors into per-connect join
graphs (e.g. `SVC0_QUERY_SVC0_ITEM0_0`), so it is **not** the customer artifact and would
make the router's `expand_connectors` a no-op. Use **rover** output for measurement — it
keeps `@connect` intact (one source graph per subgraph), which is what the router expands.

## Validation gate — results

- **`rover supergraph compose` passes for all 14 supergraphs** (7 N × 2 families). See
  `artifacts/manifest.tsv` (`compose_status` column, all `ok`).
- **Router expansion path confirmed**: `apollo-federation-cli expand` (same
  `expand_connectors` the router runs at startup) on `connectors_N64/supergraph.graphql`
  emits **768** synthetic subgraphs (`routing_url: none`), == the `@connect` count. This is
  the 1-synthetic-subgraph-per-`@connect` blowup from `00-context.md`.
- **`rover connector run` spot-check passes** (after upgrading rover 0.36.2 → **0.40.0**):
  ```
  rover connector run --schema artifacts/connectors_N1/svc0.graphql \
    --connector-id Query.svc0_item0 --variables '{"$args":{"id":"1"}}'
  # -> parses + plans the connector, then:
  #    ERROR: error sending request for url (https://svc0.example.com/items0/1)
  ```
  The connector is well-formed: rover gets all the way to issuing the HTTP request and only
  fails at the **expected** network boundary (non-routable `*.example.com` host, by design).
  (Log: `artifacts/connector_run_spotcheck.log`.) Note: rover 0.36.2 could **not** run this
  — it invoked the `supergraph-v2.14.1` plugin's `run-connector` with an unsupported
  `--path` arg (`error: unexpected argument '--path' found`); the 0.40.0 upgrade fixed it.
  Compose still works on 0.40.0 (downloads the `supergraph-v2.12.0` plugin for our pin).

## Generated artifacts (manifest)

`artifacts/manifest.tsv`:

| family | N | K | E | @connect | source graphs | synthetic subgraphs |
|--------|---|---|---|----------|---------------|---------------------|
| connectors | 1 | 8 | 4 | 12 | 1 | 12 |
| connectors | 2 | 8 | 4 | 24 | 2 | 24 |
| connectors | 4 | 8 | 4 | 48 | 4 | 48 |
| connectors | 8 | 8 | 4 | 96 | 8 | 96 |
| connectors | 16 | 8 | 4 | 192 | 16 | 192 |
| connectors | 32 | 8 | 4 | 384 | 32 | 384 |
| connectors | 64 | 8 | 4 | **768** | 64 | **768** |
| pure | 1..64 | 8 | 4 | 0 | N | N |

SDL sizes are comparable across families (connectors N64 = 248K/5902 lines vs pure N64 =
132K/5836 lines), but at runtime the connectors graph explodes to **768 synthetic
subgraphs vs pure's 64** — a **12× subgraph multiplier at the same N**. That structural
multiplier is the thing Phase 2 measures.

## Files

- `scripts/gen_schema.py` — generator.
- `scripts/compose_all.sh` — sweep driver + `artifacts/manifest.tsv`.
- `artifacts/<family>_N<n>/` — `svc*.graphql`, `supergraph.yaml`, `supergraph.graphql`, `compose.log`.
- `artifacts/connector_run_spotcheck.log` — rover connector run failure (tooling bug).

## Handoff to Phase 2

Use the **rover-composed** `artifacts/<family>_N<n>/supergraph.graphql` files (customer
path) as router input. Build the router with `dhat-heap`, start each with a minimal
`router.yaml` (ports 4000/8088), wait for ready, exit; capture peak RSS + `dhat-heap.json`
totals. Also run the federation-only dhat planner test against the same supergraphs.
Expect the connectors curve to be far steeper / superlinear vs pure. Suggested largest
run for attribution: `connectors_N64` (768 synthetic subgraphs).
