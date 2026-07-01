# Changelog

All notable changes to Apollo Composition are documented in this file.

> [!NOTE]
> Query planner changes are documented in the router [CHANGELOG](../CHANGELOG.md).

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- # [x.x.x] (unreleased) - 2023-mm-dd

> Important: X breaking changes below, indicated by **BREAKING**

## BREAKING

## Features

## Fixes

## Maintenance
## Documentation-->



# [2.16.0](https://crates.io/crates/apollo-federation/2.16.0) - 2026-06-30

Adds support for Apollo Federation v2.15.

This is the first Rust-native Apollo composition release. It introduces no new directives or composition behavior.
Rust composition generates semantically equivalent supergraphs to the previous JavaScript binaries. However, the rewrite
does surface a number of validation and error-reporting improvements, detailed below, that were previously inconsistent
or missing.

## Additional validations

### Disallow interfaces implementing `@interfaceObject`

Federation currently doesn't support this pattern and will emit `INTERFACE_OBJECT_USAGE_ERROR` error when it detects such a scenario.

### Override label validation

Composition now drops the `@override` labels if they don't reference a valid subgraph.

### External field validation

Composition now correctly validates merged directive usage on `@external` fields  and emits `MERGED_DIRECTIVE_APPLICATION_ON_EXTERNAL`
error on failures.

### Custom spec URL validation

Composition now rejects custom spec imports that specify the Apollo spec domain `https://specs.apollo.dev`. Custom spec
URLs are now checked against Apollo spec domain to prevent future collisions with new Apollo specs.

### Input object validation

Composition now validates user-provided default object values to ensure they're valid (i.e. all fields are optional
or have default values). Invalid values are removed from the supergraph.

### `@tag` validations

Composition now runs `@tag` validations as part of the main process (previously they were checked during contract variant
generation only).

### Root type inference fix

Composition ensures that it only infers default root operation types (e.g. `Mutation`) if they aren't referenced by
other schema elements.

### Stricter `FieldSet` coercion rules

`_FieldSet/FieldSet` is a custom scalar that represents a GraphQL selection set (minus brackets). While Apollo expects this
value to be a `String`, the system accepted additional values due to auto-coercion logic.

## Cosmetic changes

### Additional hints

The merge process now correctly emits hints on composition failure.

### Empty object vs expanded object

Composition now auto expands input objects explicitly specifying all the default values for the fields (e.g. `input: SortAndFilter = {}`
becomes `input: SortAndFilter = { limit: 100, sort: DESC }`).

### Value coercion

Whenever your subgraph defines a default value with a coercible value (e.g., a default value of `Int` for a field that accepts `Float`),
this value now coerces to the appropriate target type (e.g. `weight: Float = 1` becomes `weight: Float = 1.0`).

### Example queries

Due to differences in underlying data structures and libraries, this new composition version might include different
example operations in the hint messages.

### Rich error diagnostics

`apollo-compiler` provides rich error diagnostics (with line numbers and references to the schema) for GraphQL errors.

