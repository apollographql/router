# `connectors/migration/`

Home of the `connect-migrate` tool — the developer-facing CLI for
moving Apollo Connectors schemas across `connect/v0.X` spec versions.

## Why this lives in `apollo-federation`

The migration logic depends on parsing `@connect(selection: …)` under
two different `ConnectSpec` versions and comparing the resulting ASTs.
That requires reaching into JSONSelection internals that aren't part
of the public Rust API of `apollo_federation`. Co-locating the CLI in
the same crate sidesteps the need to widen that public API just for
this tool.

The release pipeline (cross-platform binaries, npm/GitHub Releases
wrapper, Claude Code plugin, etc.) lives in a separate repository
that pins a router commit and builds the binary from it.

## Layout

- `main.rs` — the `clap`-based CLI entry point. Built as the
  `connect-migrate` binary via the `[[bin]]` declaration in
  `apollo-federation/Cargo.toml`. Not a Rust module; the path is
  declared explicitly in `Cargo.toml`.
- `mod.rs` — the in-crate library surface for the migration
  subproject. Re-exports the embedded agent guide, `DiffKind` /
  `FollowedBy`, and the `analyze` module.
- `analyze.rs` — walks a project tree, finds `@connect(selection: …)`
  directives, dual-parses each under V0_3 + V0_4, and emits
  `recommendations.md` (or JSONL via `--format=json`) per the v1
  format spec. Gated on the `connect-migrate` feature so
  `apollo-parser` doesn't enter the default dep graph.
- `diff.rs` — `DiffKind` / `FollowedBy` / `JSONSelection::diff_kinds`.
  Originally introduced inside `json_selection/` for the corpus
  survey; moved here so all migration-tool surface lives in one
  directory.
- `agent_guide.md` — developer-facing migration prose, embedded into
  the binary at compile time via `include_str!`. Printed by
  `connect-migrate agent-guide`. Mirror of the canonical `SKILL.md`
  at <https://github.com/apollographql/connect-migrate>; kept in sync
  manually for now. <!-- FOLLOW-UP: pick between hash-check
  duplication, build-time fetch, or dropping the embed — see the
  FOLLOW-UP note at the bottom of SKILL.md. -->

## Building

The binary is gated behind the `connect-migrate` feature so the
`clap` and `apollo-parser` dependencies don't enter the default build
graph for `apollo-federation` library consumers:

```sh
cargo build --release --bin connect-migrate --features connect-migrate
```

## Adding new migrations

The current scope is v0.3 → v0.4 (driven by the SubSelection /
LitObject grammar unification). When a future spec version
introduces a new migration concern, the corresponding analysis
logic and agent guide should land alongside the existing material
here. Whether that means a new `migrations/<from>_to_<to>/`
sub-directory or in-place extension depends on how much of the v0.3
→ v0.4 shape generalizes — defer until there's a second case to
validate against.
