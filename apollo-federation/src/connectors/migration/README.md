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
  subproject. Re-exports the embedded agent guide and (eventually)
  analysis helpers.
- `agent_guide.md` — developer-facing migration prose, embedded into
  the binary at compile time via `include_str!`. Printed by
  `connect-migrate agent-guide`. **Interim content** — the current
  text is a verbatim port of an early design draft that assumes a
  single agent walking a chat loop. The next pass will rewrite it
  around a two-skill handoff: one skill produces a human-editable
  `recommendations.md`, the human edits decisions in place, and a
  second skill applies the chosen actions.

## Building

The binary is gated behind the `connect-migrate` feature so the
`clap` dependency doesn't enter the default build graph for
`apollo-federation` library consumers:

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
