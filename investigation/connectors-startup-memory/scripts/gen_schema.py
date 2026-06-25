#!/usr/bin/env python3
"""Parametric supergraph generator for the connectors-startup-memory investigation.

Produces two comparable supergraph *families* at parameter N (number of subgraphs):

  connectors  - each subgraph uses @source + @connect (GET + JSONSelection). Each
                @connect directive is later expanded by the router into its own
                synthetic federated subgraph, so S_synthetic ~= N * (K + E).
  pure        - same type/field counts and the same shared @key entities, but plain
                GraphQL subgraphs (no connectors). The control: isolates connectors
                overhead from raw subgraph/entity count.

Both families share E entity types (SharedJ @key(fields:"id")) defined in every
subgraph, which forces the cross-subgraph entity resolution path
(build_query_graph::handle_key, ~O(S^2)).

Syntax is grounded in this repo's validated fixtures:
  - connector subgraph SDL + supergraph.yaml shape:
      apollo-federation/src/connectors/tests/schemas/simple.{yaml,graphql}
  - entity:true connector resolver:
      apollo-federation/src/connectors/validation/test_data/keys_and_entities/valid/basic_implicit_key.graphql
  - versions: federation/v2.12 + connect/v0.3 (ConnectSpec::latest in this worktree)

Output: a directory containing one `svc{i}.graphql` per subgraph plus a rover/
federation-cli SupergraphConfig `supergraph.yaml` that references them by `file:`.
We use `file:` (not inline `sdl:`) because rover runs `${...}` env-style variable
expansion over the config YAML, which corrupts connect JSONSelection/templates like
`{$args.id}`. Schema file contents are read raw, so `file:` sidesteps that.
Compose with either:
  rover supergraph compose --config <dir>/supergraph.yaml > <out.graphql>
  cargo run -p apollo-federation-cli -- compose --config <dir>/supergraph.yaml > <out.graphql>
"""
import argparse
import os
import sys

FED_VERSION = "2.12"          # @link federation spec version
CONNECT_VERSION = "0.3"       # @link connect spec version
ROVER_FED_VERSION = "=2.12.0" # federation_version pin for rover supergraph compose


def _query_fields_connectors(i, k, e):
    lines = []
    # K plain connector fields, each returns a per-subgraph leaf type.
    for j in range(k):
        lines.append(
            f'  svc{i}_item{j}(id: ID!): Svc{i}Item{j}\n'
            f'    @connect(\n'
            f'      source: "svc_{i}"\n'
            f'      http: {{ GET: "/items{j}/{{$args.id}}" }}\n'
            f'      selection: "id name value"\n'
            f'    )'
        )
    # E entity-resolver connectors (entity: true) for the shared entity types.
    for j in range(e):
        lines.append(
            f'  svc{i}_shared{j}(id: ID!): Shared{j}\n'
            f'    @connect(\n'
            f'      source: "svc_{i}"\n'
            f'      http: {{ GET: "/shared{j}/{{$args.id}}" }}\n'
            f'      entity: true\n'
            f'      selection: "id s{i}_a s{i}_b"\n'
            f'    )'
        )
    return "\n\n".join(lines)


def _query_fields_pure(i, k, e):
    lines = []
    for j in range(k):
        lines.append(f'  svc{i}_item{j}(id: ID!): Svc{i}Item{j}')
    for j in range(e):
        lines.append(f'  svc{i}_shared{j}(id: ID!): Shared{j}')
    return "\n".join(lines)


def _leaf_types(i, k):
    return "\n\n".join(
        f'type Svc{i}Item{j} {{\n  id: ID!\n  name: String\n  value: Int\n}}'
        for j in range(k)
    )


def _shared_types(i, e):
    # Each subgraph contributes distinct fields (s{i}_a/s{i}_b) to every shared
    # entity, so the merged entity is resolvable by all N subgraphs -> handle_key.
    return "\n\n".join(
        f'type Shared{j} @key(fields: "id") {{\n  id: ID!\n  s{i}_a: String\n  s{i}_b: String\n}}'
        for j in range(e)
    )


def connectors_sdl(i, k, e):
    return (
        'extend schema\n'
        f'  @link(url: "https://specs.apollo.dev/federation/v{FED_VERSION}", import: ["@key"])\n'
        f'  @link(url: "https://specs.apollo.dev/connect/v{CONNECT_VERSION}", import: ["@connect", "@source"])\n'
        f'  @source(name: "svc_{i}", http: {{ baseURL: "https://svc{i}.example.com" }})\n\n'
        'type Query {\n'
        f'{_query_fields_connectors(i, k, e)}\n'
        '}\n\n'
        f'{_leaf_types(i, k)}\n\n'
        f'{_shared_types(i, e)}\n'
    )


def pure_sdl(i, k, e):
    return (
        'extend schema\n'
        f'  @link(url: "https://specs.apollo.dev/federation/v{FED_VERSION}", import: ["@key"])\n\n'
        'type Query {\n'
        f'{_query_fields_pure(i, k, e)}\n'
        '}\n\n'
        f'{_leaf_types(i, k)}\n\n'
        f'{_shared_types(i, e)}\n'
    )


def build_config_files(family, n, k, e, out_dir):
    """Write svc{i}.graphql files + supergraph.yaml into out_dir. Returns yaml path."""
    if family == "connectors":
        sdl_fn = connectors_sdl
    elif family == "pure":
        sdl_fn = pure_sdl
    else:
        raise ValueError(family)

    os.makedirs(out_dir, exist_ok=True)
    out = [f"federation_version: {ROVER_FED_VERSION}", "subgraphs:"]
    for i in range(n):
        fname = f"svc{i}.graphql"
        with open(os.path.join(out_dir, fname), "w") as f:
            f.write(sdl_fn(i, k, e))
        out.append(f"  svc{i}:")
        out.append(f"    routing_url: https://svc{i}.example.com")
        out.append("    schema:")
        out.append(f"      file: {fname}")
    yaml_path = os.path.join(out_dir, "supergraph.yaml")
    with open(yaml_path, "w") as f:
        f.write("\n".join(out) + "\n")
    return yaml_path


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--family", choices=["connectors", "pure"], required=True)
    p.add_argument("--n", type=int, required=True, help="number of subgraphs")
    p.add_argument("--k", type=int, default=4, help="plain connector/query fields per subgraph")
    p.add_argument("--e", type=int, default=2, help="shared entity types (cross-subgraph @key)")
    p.add_argument("--out-dir", required=True, help="directory to write svc*.graphql + supergraph.yaml")
    args = p.parse_args()

    yaml_path = build_config_files(args.family, args.n, args.k, args.e, args.out_dir)
    connects = args.n * (args.k + args.e) if args.family == "connectors" else 0
    sys.stderr.write(
        f"[gen] family={args.family} N={args.n} K={args.k} E={args.e} "
        f"subgraphs={args.n} @connect_directives={connects} -> {yaml_path}\n"
    )
    print(yaml_path)


if __name__ == "__main__":
    main()
