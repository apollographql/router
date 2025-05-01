# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build, Lint & Test Commands

- `cargo build --all-targets` - Build the project
- `cargo build -p apollo-federation` - Build only the federation crate
- `cargo xtask lint` - Run formatting checks and clippy
- `cargo xtask lint --fmt` - Fix formatting issues
- `cargo test -p apollo-federation` - Run all apollo-federation tests
- `cargo test -p apollo-federation -- test_name --exact --nocapture` - Run a single federation test
- `cargo test -p apollo-federation schema_upgrader` - Run tests for the schema_upgrader module
- `cd apollo-federation && cargo test` - Run all tests (from federation directory)

## Apollo Federation Code Style and Guidelines

- Uses Rust 2024 edition style (configured in rustfmt.toml)
- Imports: Group with `imports_granularity=Item` and `group_imports=StdExternalCrate`
- Error handling: Federation uses a custom `FederationError` type with descriptive error messages
- Tests: Use snapshot testing with insta for schema composition tests
- Code organization: The apollo-federation crate implements GraphQL federation functionality:
  - schema: Schema composition and validation
  - query_plan: Federation query planning
  - query_graph: Internal representation of queries
  - operation: GraphQL operations processing
  - subgraph/supergraph: Federation subgraph and supergraph handling

When modifying federation code, ensure full test coverage, especially for schema upgrader changes.