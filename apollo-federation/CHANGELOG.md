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

# [2.16.x](unreleased) - Unreleased

## 🐛 Fixes

### Upgrading federation 1 subgraphs no longer clones subgraph metadata once per type ([PR #9946](https://github.com/apollographql/router/pull/9946))

Composition constructs a schema upgrader as soon as **one** input subgraph is a
federation 1 schema. The upgrader indexes the object and interface types of every
subgraph in the composition — including the federation 2 subgraphs it will never
upgrade, because the upgrade rules have to look at what those subgraphs define —
and each index entry carried a full clone of the defining subgraph's
`SubgraphMetadata`. The index therefore cost the sum, over all subgraphs, of (that
subgraph's type count) × (that subgraph's own metadata size), and the great majority
of it was spent on behalf of subgraphs that are passed through untouched.

The index now records only where each type is defined. The schema and the metadata a
lookup needs are read from the subgraph the entry names, which the upgrader already
holds, so exactly one metadata value exists per subgraph instead of one per (type,
subgraph) pair. Composition output is unchanged.

On a generated fixture of 40 federation 2 subgraphs, each declaring 120 entity types
and 12 interfaces, beside a single 120-byte federation 1 subgraph, peak
requested-live heap across composition drops from 1.35 GB to 0.14 GB — a 9.3×
reduction. Composing those same 40 federation 2 subgraphs on their own — no
federation 1 subgraph, so no upgrader is built — peaks at 0.14 GB, which is the
measure of what the one small subgraph used to cost: it added 1.21 GB of peak heap
before this change, and adds 0.15 MB after it. The effect grows with the size and
number of the federation 2 subgraphs, since they are what the index was being built
over.

By [@martijnwalraven](https://github.com/martijnwalraven) in <https://github.com/apollographql/router/pull/9946>

### Reject `@external` fields on nested `@key` paths with cross-subgraph `@requires` ([PR #9832](https://github.com/apollographql/router/pull/9832))

When a subgraph declared a nested `@key` (e.g., `@key(fields: "id u { x }")`) where
fields along the key path were marked `@external`, and also had a `@requires` that
pulled data from a different subgraph, the query planner would fail at planning time
with an internal error. Composition passed without errors, so there was no warning
until the query failed at runtime.

Composition now catches this as a `SATISFIABILITY_ERROR`. To fix affected schemas,
replace `@external` with `@shareable` on key-path fields, which is the intended
Federation 2 pattern.

By [@tninesling](https://github.com/tninesling) in <https://github.com/apollographql/router/pull/9832>

### Propagate directives from `@interfaceObject` fields to `@external` implementations ([PR #9831](https://github.com/apollographql/router/pull/9831))

When an implementation re-declares a field as `@external` (e.g. to reference it in `@requires`), the field's only resolvable definition lives on the abstracting `@interfaceObject`. Directives like `@tag` applied there were not being propagated to the implementation's copy in the supergraph.

During `add_interface_object_fields`, detect implementation fields where every `@join__field` is `external: true` and the field is provided by an `@interfaceObject`, then copy applicable directives onto the implementation field.

By [@dariuszkuc](https://github.com/dariuszkuc) in <https://github.com/apollographql/router/pull/9831>

### Fix composition field merging when subtyping ([PR #9751](https://github.com/apollographql/router/pull/9751))

When composition merges fields with different return types, it was previously allowing nullable types to be considered subtypes of non-null supertypes. The resulting supergraph schema could cause query plan execution to error if the subgraph returns null at runtime. This bug has been fixed, and composition will now appropriately error.

By [@sachindshinde](https://github.com/sachindshinde) in <https://github.com/apollographql/router/pull/9751>

### Skip `@requires` field set validation during fed v1 schema upgrade ([PR #9722](https://github.com/apollographql/router/pull/9722))

Updates `@requires` validation logic to allow type selection conditions in the field selections that are only valid
against the supergraph. `@requires` is now partially validated against subgraph schema during subgraph upgrade process
and fully validated against supergraph schema during the merge process.

By [@dariuszkuc](https://github.com/dariuszkuc) in <https://github.com/apollographql/router/pull/9722>

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
