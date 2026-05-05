# @mapping Directive

This document describes the `@mapping` directive for Apollo Connectors - enabling reusable JSON-to-GraphQL field mappings.

## Overview

The `@mapping` directive allows defining field mappings once on a type and referencing them via `...TypeName` spread syntax in `@connect` selection strings.

## Syntax

```graphql
directive @mapping(selection: String, as: String) repeatable on OBJECT
```

**Arguments:**
- `selection` (optional): A JSONSelection string defining the field mapping
- `as` (optional): An alias for the mapping, allowing multiple mappings per type

## Usage

### Auto-map Mode

When no `selection` is provided, mapping is generated from field names:

```graphql
type User @mapping {
  id: ID!
  name: String!
  email: String!
}
# Equivalent to: @mapping("id name email")
```

### Explicit Selection

```graphql
type Post @mapping("""
  id
  title
  authorName: author.name
  createdAt: metadata.created_at
""") {
  id: ID!
  title: String!
  authorName: String!
  createdAt: String!
}
```

### Multiple Mappings with Aliases

```graphql
type User @mapping(as: "UserBasic", "id name")
         @mapping(as: "UserFull", "id name email profile { avatar }") {
  id: ID!
  name: String!
  email: String!
  profile: Profile
}

# Reference: "...UserBasic" or "...UserFull"
```

### Referencing in @connect

```graphql
type Query {
  user(id: ID!): User @connect(
    source: "api"
    http: { GET: "/users/{$args.id}" }
    selection: "...User"
  )

  users: [User!]! @connect(
    source: "api"
    http: { GET: "/users" }
    selection: "items { ...User }"
  )
}
```

## Parameterized Mappings

Mappings can declare parameters — any `$variable` in the selection that is not a reserved runtime namespace (`$this`, `$args`, `$config`, `$status`, `$context`, `$request`, `$response`, `$env`, `$batch`).

```graphql
type User @mapping(selection: "id friends->slice(0, $count)") {
  id: ID!
  friends: [User!]!
}

type Query {
  recentFriends: User @connect(selection: "...User(count: 3)")
  allFriends: User @connect(selection: "...User(count: 50)")
}
```

### Rules

- Parameters are **inferred** from the selection AST (no explicit declaration needed).
- All parameters are **required** at every spread site.
- Substitution is **schema-load-time** (structural AST replacement).
- v1 constraint: arguments must be **literals only** (string, number, boolean, null).
- Nested forwarding rejected: `...Inner(count: $count)` inside a mapping is an error.
- Argument names must not conflict with reserved runtime namespaces.

### Key types

| Type | File | Purpose |
|------|------|---------|
| `SpreadArg` | `json_selection/parser.rs` | Single `name: value` argument |
| `SpreadArgs` | `json_selection/parser.rs` | Parsed argument list with range |
| `MappingDefinition.parameters` | `mapping_registry.rs` | Inferred parameter set |

### Substitution flow

1. `compute_parameters()` walks the selection's `external_var_paths()`, filters out `Namespace` matches.
2. At expansion time, `build_substitutions()` validates args against parameters.
3. `expand_path_list` (Var arm) and `expand_lit_expr` (Path arm) perform the replacement.

## Method Calls

Methods (`->`) ARE allowed in `@mapping` selections:

```graphql
type User @mapping("""
  id
  name: fullName->lowercase
  tags: categories->first
""") { ... }
```

**Note:** `...Type->method()` is invalid syntax (parser rejects it).

## Architecture

### Key Components

| Component | File | Purpose |
|-----------|------|---------|
| `MappingDirectiveArguments` | `spec/mapping.rs` | Parsed directive arguments |
| `MappingRegistry` | `mapping_registry.rs` | Stores and expands mappings |
| `SpreadNamed` | `json_selection/parser.rs` | Parser variant for `...TypeName(args)` |

### Data Flow

1. **Schema Loading**: Parse SDL, extract `@mapping` directives
2. **Registry Building**: `MappingRegistry::from_schema()` collects all mappings
3. **Connector Processing**: `expand_selection()` replaces spreads with definitions

### Expansion Algorithm

```rust
fn expand_sub_selection(&self, sub: &SubSelection, expanding: &mut HashSet<String>) {
    for named in &sub.selections {
        match &named.prefix {
            NamingPrefix::SpreadNamed { name, .. } => {
                // 1. Check for circular reference
                // 2. Look up the mapping
                // 3. Mark as expanding (cycle detection)
                // 4. Recursively expand
                // 5. Inline expanded selections
            }
            _ => { /* Recursively expand nested selections */ }
        }
    }
}
```

## Testing

```bash
# Run all mapping tests
cargo test -p apollo-federation mapping

# Run specific test
cargo test -p apollo-federation test_expand_simple_spread
```

## Error Messages

| Error | Cause |
|-------|-------|
| `Unknown mapping reference: ...FooType` | No `@mapping` on type `FooType` |
| `Circular reference detected` | Mapping references itself |
| `Failed to parse @mapping selection` | Invalid selection syntax |
| `must be a field selection, not a path` | Selection starts with `$` |
| `passes arguments, but mapping has no parameters` | Args on parameterless mapping |
| `is missing required argument` | Spread omits a parameter |
| `passes unknown argument` | Arg name not in mapping's parameters |
| `provides duplicate argument` | Same arg name passed twice |
| `uses a variable/path as argument value` | Non-literal arg (v1 restriction) |
| `conflicts with reserved runtime variable` | Arg name matches a Namespace |
| `must start with an uppercase ASCII letter` | `as:` value not SpreadNamed-compatible |

## Diagrams

### NamingPrefix AST Structure

The `NamingPrefix` enum defines the four AST node types for selection prefixes. `SpreadNamed` is the variant added for `@mapping` references.

![NamingPrefix AST](diagrams/naming-prefix-ast.svg)

### SpreadNamed Data Flow

End-to-end data flow showing how `...TypeName` is parsed, expanded via the registry, and applied to JSON at runtime.

![SpreadNamed Data Flow](diagrams/spread-named-dataflow.svg)

### SpreadNamed Processing Sequence

Sequence diagram showing the interaction between Schema, Parser, MappingRegistry, and JSON application.

![SpreadNamed Sequence](diagrams/spread-named-sequence.svg)

### Schema Parsing and Composition

How the router parses subgraph schemas, extracts `@mapping` directives, builds the MappingRegistry, and expands `...TypeName` spreads in `@connect` selections.

![Schema Parsing](diagrams/connector-schema-parsing.svg)

### Router Initialization

Loading connectors into the Router service, including supergraph expansion and plugin setup.

![Router Init](diagrams/connector-router-init.svg)

### Request Execution

From GraphQL query through query planning, connector dispatch, HTTP request building, to external API call.

![Request Execution](diagrams/connector-request-execution.svg)

### Response Handling

Transforming HTTP responses back to GraphQL data via JSONSelection mapping (applied after expansion).

![Response Handling](diagrams/connector-response-handling.svg)

---

## Implementation History

### Commit 1: Core Implementation

**Files changed:**
| File | Change |
|------|--------|
| `spec/mod.rs` | Added `ConnectSpec::V0_5` (or V0_4) |
| `spec/type_and_directive_specifications.rs` | `mapping_directive_spec()` |
| `spec/mapping.rs` | **NEW** - `MappingDirectiveArguments` |
| `mapping_registry.rs` | **NEW** - `MappingRegistry` |
| `json_selection/parser.rs` | `NamingPrefix::SpreadNamed` |
| `json_selection/apply_to.rs` | Handle `SpreadNamed` |
| `json_selection/location.rs` | Handle `SpreadNamed` |
| `json_selection/pretty.rs` | Handle `SpreadNamed` |
| `json_selection/mod.rs` | `PathSelection::empty()` |
| `models.rs` | `mapping_directive_name` |

### Commit 2: Supergraph Expansion Fix

**Problem:** Router failed with `@mapping does not refer to an existing directive`.

**Solution:** Added `mapping_directive_name` to `directive_deny_list` in `expand/mod.rs`.

| File | Change |
|------|--------|
| `expand/mod.rs` | Added to `directive_deny_list` |
| `supergraph/mod.rs` | Added `test_join_directives_v0_5_mapping` |
