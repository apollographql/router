# Phase 0 handoff — source-aware QP spikes

Context: rev 2 of the source-aware query-planner proposal (inline at brane
narrative `claude-h371fb7k:31`; grilling record in `claude-jmc9ipcf`). This
worktree is `benjamn/source-aware-qp` = `origin/dev` (`14335e254`) +
`benjamn/precise-connector-output-shapes` (shape `0.8.0-preview.2` +
`JSONSelection::shape_with_vars`). No spike code has landed yet; this file is
the complete grounding for an Opus agent to implement Spikes A and B.

## Survey conclusions (verified against this tree)

### Query-graph seam
- An edge condition is just `Option<Arc<SelectionSet>>` on `QueryGraphEdge` —
  connector `$this`/key inputs map onto it directly; no new type needed.
- Reuse `KeyResolution` for connector-entering edges; a new transition variant
  would fan out across ~7 exhaustive matches.
- Typed `SourceId` retrofit funnels through ~6 struct definition sites; the
  per-source fetch-merge/reuse hooks are 5 `subgraph_name` equality sites in
  `fetch_dependency_graph.rs`.

### Connector inputs (the Spike A ground truth)
- Deep input requirements come from `Connector::variable_references()`
  (`apollo-federation/src/connectors/models.rs:344`) which chains transport
  refs + response-selection external var paths. Each
  `VariableReference<Namespace>` carries a full `SelectionTrie`
  (`connectors/variable.rs:152`) — NOT just top-level keys. The shallow
  `request_variable_keys` / `response_variable_keys` maps are top-level-only
  and unsuitable for condition derivation.
- Namespaces (`connectors/variable.rs:90`): `Args, Config, Context, Status,
  This, Batch, Request, Response, Env`.
- Classification for the planner:
  - `$args` / `EntityResolver::Explicit` → operation-satisfiable (client
    provides via field arguments).
  - `$this` (`Implicit`, `TypeSingle`), `$batch` (`TypeBatch`) → parent-data
    requirements = **edge conditions** for the planner.
  - `$config`, `$env`, `$context`, `$request`, `$status`, `$response` →
    environment; never planner-visible conditions.
- `EntityResolver` variants: `models.rs:169` (Explicit / Implicit / TypeBatch /
  TypeSingle), determined by `determine_entity_resolver` (called from
  `from_directives`, `models.rs:267`).

### How expansion fabricates keys today (what conditions must reproduce)
- `Connector::resolvable_key` (`models.rs:359`): per entity-resolver variant,
  filters `variable_references()` to one namespace (`Args` for Explicit,
  `This` for Implicit/TypeSingle, `Batch` for TypeBatch), merges the
  `SelectionTrie`s, and parses the merged trie as a `FieldSet` against the
  original schema — see `make_key_field_set_from_variables`
  (`models/keys.rs:14`). Key type: output type (`base_type_name`) for
  Explicit/TypeBatch/TypeSingle; parent type (`parent_type_name`) for
  Implicit.
- `process_outputs` (`expand/mod.rs:588`):
  - No resolvable key + `Implicit` → **singleton entity key**
    `@key(fields: "__typename")` (`expand/mod.rs:604`,
    `add_singleton_entity_key`) — the "namespace container" pattern.
  - No key otherwise → `copy_interface_object_keys` (`expand/mod.rs:607`).
  - With a key: walks key-field child types (`walk_type_with_shape`), then
    **inserts sibling fields the connector doesn't return** into the expanded
    type when the key depends on them (`expand/mod.rs:635-667`), then attaches
    the `@key` directive — including copying an interface's key onto all
    implementers (`expand/mod.rs:685-703`).

### Harness (the Spike B ground truth)
- `apollo-federation/src/correctness/` (`check_plan` / `compare_operations`)
  is a modern semantic plan-comparison engine already in-tree.
- The old dual-planner differ `plan_compare.rs` (removed with the JS planner,
  PR #6418 era) is recoverable at `232f40da4^` as the reference spec for
  Parallel-as-set / selection-sorting equivalence rules.
- Corpus: 204 supergraphs in `apollo-federation/tests/query_plan/supergraphs/`
  plus the connector expand fixtures
  (`apollo-federation/src/connectors/expand/tests/schemas/expand/`).

## Spike A brief — conditions-derivation module

New module `apollo-federation/src/connectors/source_aware/` (name not final):

1. `ConnectorInputClassification`: for a `Connector`, partition every
   `variable_references()` entry into `OperationSatisfiable` ($args),
   `ParentData` ($this / $batch, with merged `SelectionTrie` per namespace),
   and `Environment` (everything else). Use the deep tries, not
   `*_variable_keys`.
2. Derive the planner-facing condition `FieldSet` from the ParentData
   partition — same merge as `make_key_field_set_from_variables`, but
   covering the fabricated cases: `Implicit` with no `$this` refs must yield
   the `__typename` singleton condition, and sibling-field dependencies must
   be representable even when the connector's selection doesn't return them
   (in the source-aware world there is no expanded schema to insert them
   into — the condition SelectionSet is validated against the *original*
   schema, which already has those fields).
3. Differential test: for every connector in every expand fixture, derived
   condition ≡ `resolvable_key(schema)` output (serialize both and compare),
   and the singleton / interface-object fallbacks match `process_outputs`
   behavior.

## Spike B brief — `plan-diff` harness

Federation-CLI subcommand `plan-diff`:
- Input: a supergraph + operation corpus; plan each operation under two
  configurations; classify each result as
  identical / equivalent / different / error.
- Equivalence via the `correctness/` engine (`compare_operations`); consult
  old `plan_compare.rs` (at `232f40da4^`) for Parallel-as-set and
  selection-sorting rules.
- Output: JSON report + human-readable structural diff; thin insta wrapper
  for CI over the 204-supergraph corpus.
- Leave a pluggable "mode B" seam so the second configuration can later be
  the source-aware planner rather than an expansion variant.

## Process constraints (operator-set)
- Work stays in this worktree on `benjamn/source-aware-qp`.
- No subagents; commit checkpoints early and often — prior sessions lost
  in-flight work three times.
- Never delete anything (brane golden rule).
