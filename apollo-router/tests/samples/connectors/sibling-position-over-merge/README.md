# Connector output shape: same type at two positions is over-merged

A single connector whose selection reaches the same type at two different
positions, returning different fields at each, plans as though every position
returns the union of both. Fields that the connector does not return at a given
position come back `null`, with no error anywhere in the pipeline.

No recursion is involved. This composes cleanly and is not caught by any
validation.

## The subgraph

`Query.users` returns `User`, and `User` has two fields of type `Person`:

```graphql
type Query {
  users: [User]
    @connect(
      source: "example"
      http: { GET: "/users" }
      selection: "id manager { id name } reports { id }"
    )
}
```

The selection returns `Person` two ways:

- under `manager`, with `id` and `name`
- under `reports`, with `id` only

## What happens

Under expansion, which is the default planning path:

| query | response |
|---|---|
| `{ users { manager { name } } }` | `"Ada"` |
| `{ users { reports { name } } }` | **`null`** |

Same field, same type, different position, different answer. The upstream
response in `http_snapshots.json` is exactly what the selection describes:
`reports` elements carry only `id`.

With `experimental_connectors_source_aware: true`, both positions resolve
correctly:

| query | response | requests |
|---|---|---|
| `{ users { manager { name } } }` | `"Ada"` | `GET /users` |
| `{ users { reports { name } } }` | `"Grace"` | `GET /users`, `GET /people/2` |
| `{ users { manager { title } } }` | `"Director"` | `GET /users`, `GET /people/9` |

This sample runs **both** paths in one test (via `ReloadConfiguration`), so the
divergence between them is the artifact rather than something a reader has to
reconstruct from two directories.

The values are chosen to be self-proving. `GET /people/9` returns
`"Ada (via resolver)"`, not `"Ada"`, so `manager { name }` coming back as plain
`"Ada"` shows the restriction did *not* manufacture a needless entity fetch at a
position the connector can serve. Conversely `"Grace"` appears nowhere in the
`/users` response, so it can only have come from the resolver.

`manager { title }` is the case that distinguishes a genuinely per-position
restriction from one that merely fixed `reports`: `title` is returned at neither
position, so even `manager` must re-enter.

## Why

Expansion synthesizes one subgraph per `@connect` directive, and therefore one
`Person` type per synthetic subgraph. It cannot hold two field sets for one
`(subgraph, type)` pair, so it unions them. The composed-and-expanded schema
attributes `Person.name` to `connectors_Query_users_0`:

```graphql
type Person
  @join__type(graph: CONNECTORS_QUERY_PERSON_0, key: "id")
  @join__type(graph: CONNECTORS_QUERY_USERS_0)
{
  id: ID!
    @join__field(graph: CONNECTORS_QUERY_PERSON_0)
    @join__field(graph: CONNECTORS_QUERY_USERS_0)
  name: String
    @join__field(graph: CONNECTORS_QUERY_PERSON_0)
    @join__field(graph: CONNECTORS_QUERY_USERS_0)   # <-- over-claimed
  title: String
    @join__field(graph: CONNECTORS_QUERY_PERSON_0)
}
```

The planner then believes the `users` connector can serve `Person.name` from
either position, and emits a single fetch:

```
QueryPlan {
  Fetch(service: "connectors_Query_users_0") {
    { users { reports { name } } }
  },
}
```

`Query.person` is an entity resolver that could have supplied `name` through a
follow-up `_entities` fetch. The planner has no reason to reach for it, because
the join metadata says the field is already available.

Both positions belong to the **same connector**, so no partitioning of
connectors into subgraphs separates them. One synthetic subgraph per `@connect`
is already the finest such partition. Representing this correctly requires more
than one node per `(subgraph, type)`.

## How source-aware planning fixes it

`connect_graph::restrict_connector_reachability` walks the connector's output
shape recursively alongside the query graph and gives each position its own
restricted copy of the landing type, with a `KeyResolution` edge back to the full
node:

```
Person          id, name, title      (full node)
Person copy     id, name             <- the `manager` position
Person copy     id                   <- the `reports` position
User copy       id, manager->…, reports->…
```

Two flavours of `Person` in one subgraph, which the query graph's
`IndexMap<source, IndexMap<type, IndexSet<NodeIndex>>>` already accommodates —
the same mechanism `@provides` copies use. The planner then reaches `name` under
`reports` only through the re-entry, and emits the `_entities` fetch.

This is the **first** fixture in the tree where source-aware planning must
*disagree* with expansion. Every other source-aware test asserts byte-for-byte
parity with the expansion path, because expansion is the oracle for them. Here
expansion is wrong, so parity would be the bug. Do not "fix" this sample by
making the two paths agree.

## When expansion itself is fixed

The expansion-path expectations in `plan.json` (`reports { name }` → `null`)
encode a **defect deliberately**. If someone fixes expansion to represent
per-position field sets, this sample should fail loudly rather than be quietly
updated: that failure is the signal the defect is gone, and it should be read as
such before the expectations are changed.

## Reproducing the schema

`supergraph.graphql` is checked in, composed from `connectors.graphql` with:

```sh
rover supergraph compose --config ./supergraph.yaml > ./supergraph.graphql
```

Composition succeeds with no errors and no hints. Versions are pinned in
`supergraph.yaml`: federation 2.13 with connect v0.4, which is the minimum
federation version connect v0.4 requires (see `CONNECT_VERSIONS` in
`apollo-federation/src/connectors/spec/mod.rs`).

## Running

This is a `tests/samples` case, driven by the sample runner:

```sh
cargo test -p apollo-router --test samples --features snapshot sibling
```

**`--features snapshot` is not optional.** Samples with `"snapshot": true` are
skipped without it (`samples_tests.rs:127`), silently and without a message — if
`--list` shows only four samples, the feature is missing. Similarly, `Plan` and
`Action` use `serde(deny_unknown_fields)`, so a stray key in `plan.json` skips the
whole sample with no explanation.
