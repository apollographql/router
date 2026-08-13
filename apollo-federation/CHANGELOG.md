# Changelog

All notable changes to Apollo Composition are documented in this file.

> [!NOTE]
> Query planner changes are documented in the router [CHANGELOG](../CHANGELOG.md).

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- # [x.x.x] (unreleased) - 2023-mm-dd

> Important: X breaking changes below, indicated by **BREAKING**

## ❗ BREAKING CHANGES ❗
## 🚀 Features
## 🐛 Fixes
## 🛠 Maintenance
## 📚 Documentation
-->

# [2.16.2](https://crates.io/crates/apollo-federation/2.16.2) - 2026-08-13

## 🐛 Fixes

### Propagate directives from `@interfaceObject` fields to `@external` implementations ([PR #9831](https://github.com/apollographql/router/pull/9831))

When an implementation re-declares a field as `@external` (e.g. to reference it in `@requires`), the field's only resolvable definition lives on the abstracting `@interfaceObject`. Directives like `@tag` applied there were not being propagated to the implementation's copy in the supergraph.

During `add_interface_object_fields`, detect implementation fields where every `@join__field` is `external: true` and the field is provided by an `@interfaceObject`, then copy applicable directives onto the implementation field.

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/9831

### Reject nullable-to-non-null field merges during composition ([PR #9751](https://github.com/apollographql/router/pull/9751))

When composition merges fields with different return types, it was previously allowing nullable types to be considered subtypes of non-null supertypes. The resulting supergraph schema could cause query plan execution to error if the subgraph returns null at runtime. This bug has been fixed, and composition will now appropriately error.

By [@sachindshinde](https://github.com/sachindshinde) in https://github.com/apollographql/router/pull/9751

### Skip `@requires` field set validation during fed v1 schema upgrade ([PR #9722](https://github.com/apollographql/router/pull/9722))

Updates `@requires` validation logic to allow type selection conditions in the field selections that are only valid
against the supergraph. `@requires` is now partially validated against subgraph schema during subgraph upgrade process
and fully validated against supergraph schema during the merge process.

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/9722

# [2.16.0](https://crates.io/crates/apollo-federation/2.16.0) - 2026-06-30

Adds support for Apollo Federation v2.15.

Composition is now written in Rust. No new directives or composition behavior were introduced. Your supergraphs are semantically equivalent to those built with the previous version. The main benefits are faster builds and significantly improved error messages.

Because the Rust implementation is more rigorous, composition now catches several categories of schema problems that were previously inconsistent or missing. If you upgrade and encounter new errors, the following sections explain what to fix.

#### New validations

##### Interfaces implementing `@interfaceObject` now fail explicitly

Federation doesn't support interfaces implementing `@interfaceObject` interfaces. If your schema uses this pattern, composition now reports `INTERFACE_OBJECT_USAGE_ERROR`.

##### Invalid `@override` labels are dropped

If an `@override` directive's `label` references a subgraph name that doesn't exist in your graph, composition now drops that label. Review your `@override` usage to ensure all labels are valid subgraph names.

##### Merged directives on `@external` fields are rejected

Applying a merged directive to an `@external` field now produces a `MERGED_DIRECTIVE_APPLICATION_ON_EXTERNAL` error. Review your `@external` field definitions.

##### Custom spec URLs can't use the Apollo domain

Custom specifications can no longer import from `https://specs.apollo.dev`. This prevents future conflicts with new Apollo specifications.

##### Invalid input object defaults are removed

Default values for input objects are now validated at composition time. If a default object value is missing required fields, composition removes it from the supergraph.

##### `@tag` validation runs during composition

`@tag` errors now surface during the main composition process so you can catch tag problems at build time.

##### Root type inference fix

Composition ensures that it only infers default root operation types (e.g. `Mutation`) if they aren't referenced by
other schema elements.

##### `FieldSet` arguments must be strings

The `_FieldSet` scalar no longer accepts non-string values through automatic coercion. Make sure all `fields` arguments on `@key`, `@requires`, and `@provides` directives use quoted strings.

#### Improved error messages

##### Hints are emitted on composition failure

The composition process now emits hints even when composition fails, giving you more context to diagnose what went wrong.

##### Input object defaults are fully expanded

When an input object has a default value of `{}`, composition now expands it to list all field defaults explicitly — for example, `{}` becomes `{ limit: 100, sort: DESC }`.

##### Default values are normalized to their correct types

If a field's default value is a coercible type (for example, an integer default on a `Float` field), composition normalizes it — for example, `weight: Float = 1` becomes `weight: Float = 1.0`.

##### Errors include line numbers and schema references

Error messages now include line numbers and point to the relevant parts of your schema, making it faster to locate and fix problems.

