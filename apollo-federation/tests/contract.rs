//! End-to-end tests for contract filtering.
//!
//! The per-step unit tests in `src/contract/` each drive one transform against a
//! hand-built schema. These exercise [`filter_schema`] itself: the `@link` injection in
//! step 1 and the metadata re-derivation that follows it, the handoff from the positions
//! API to direct `Schema` edits, the cascade between steps, and the validation of the
//! result. They also go through the public API, so the crate's external surface is
//! typechecked.

use apollo_federation::ApiSchemaOptions;
use apollo_federation::composition::Satisfiable;
use apollo_federation::composition::Supergraph;
use apollo_federation::contract::ContractFilters;
use apollo_federation::contract::errors::FilterStep;
use apollo_federation::contract::errors::TransformError;
use apollo_federation::contract::filter_schema;

/// A supergraph that links neither the tag nor the inaccessible spec, so filtering has to
/// add both. Ported from the TypeScript suite's `restoreDefaults.test.ts`, which is the
/// only test there that drives the whole pipeline; the default values on `AnInput`,
/// `OtherInput` and `@join__type` are what that test was written to protect.
const SUPERGRAPH_WITHOUT_TAG_SPEC: &str = r#"
schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
{
  query: Query
}

directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

directive @join__graph(name: String!, url: String!) on ENUM_VALUE

directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

type A
  @join__type(graph: A, key: "id")
{
  id: ID!
  field(input: OtherInput): String!
}

input AnInput
  @join__type(graph: A)
{
  currency: [CurrencyCode!]! = []
  label: [String!]! = []
}

scalar CurrencyCode
  @join__type(graph: A)

scalar join__FieldSet

enum join__Graph {
  A @join__graph(name: "A", url: "http://A")
}

scalar link__Import

enum link__Purpose {
  SECURITY

  EXECUTION
}

input OtherInput
  @join__type(graph: A)
{
  a: AnInput = {}
}

type Query
  @join__type(graph: A)
{
  a: A!
}
"#;

/// A supergraph that already links tag v0.3 and inaccessible v0.2, with tagged elements to
/// filter on. The inaccessible `@link` deliberately omits `for: SECURITY`, as old Fed 2
/// composition versions did.
const TAGGED_SUPERGRAPH: &str = r#"
schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
  @link(url: "https://specs.apollo.dev/tag/v0.3")
  @link(url: "https://specs.apollo.dev/inaccessible/v0.2")
{
  query: Query
}

directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

directive @join__graph(name: String!, url: String!) on ENUM_VALUE

directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

directive @tag(name: String!) repeatable on FIELD_DEFINITION | INTERFACE | OBJECT | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA

directive @inaccessible on FIELD_DEFINITION | INTERFACE | OBJECT | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

scalar join__FieldSet

enum join__Graph {
  A @join__graph(name: "A", url: "http://A")
}

scalar link__Import

enum link__Purpose {
  SECURITY

  EXECUTION
}

type Query
  @join__type(graph: A)
{
  public: Product! @tag(name: "public")
  internal: Secret! @tag(name: "internal")
}

type Product
  @join__type(graph: A)
  @tag(name: "public")
{
  id: ID!
  name: String!
}

type Secret
  @join__type(graph: A)
  @tag(name: "internal")
{
  id: ID!
  onlyReachableFromSecret: Orphan!
}

type Orphan
  @join__type(graph: A)
{
  id: ID!
}
"#;

fn filter(
    sdl: &str,
    include: &[&str],
    exclude: &[&str],
    hide_unreachable_types: bool,
) -> Supergraph<Satisfiable> {
    let filters = ContractFilters::new(
        include.iter().copied(),
        exclude.iter().copied(),
        hide_unreachable_types,
    )
    .expect("test filters should not overlap");
    filter_schema(sdl, &filters).expect("filtering should succeed")
}

/// The API schema a gateway would serve for this contract variant.
fn api_schema(supergraph: &Supergraph<Satisfiable>) -> String {
    supergraph
        .to_api_schema(ApiSchemaOptions::default())
        .expect("filtered supergraph should produce an API schema")
        .schema()
        .serialize()
        .to_string()
}

/// Step 1 has to link both specs and add their definitions, then later steps have to
/// find them through metadata that only exists because step 1 re-collected it.
#[test]
fn links_tag_and_inaccessible_specs_when_absent() {
    let supergraph = filter(SUPERGRAPH_WITHOUT_TAG_SPEC, &[], &[], false);
    let sdl = supergraph.schema().schema().serialize().to_string();

    let schema_definition = sdl
        .lines()
        .take_while(|line| !line.starts_with('}'))
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(schema_definition);
}

/// Default values survive the round trip. The TypeScript pipeline needs a dedicated
/// record/restore pass for this because it prints and re-parses through graphql-js; this
/// one edits the parsed schema in place, so there is nothing to restore.
#[test]
fn preserves_input_default_values() {
    let supergraph = filter(SUPERGRAPH_WITHOUT_TAG_SPEC, &[], &[], false);
    let sdl = supergraph.schema().schema().serialize().to_string();

    assert!(
        sdl.contains("currency: [CurrencyCode!]! = []"),
        "empty list default should survive:\n{sdl}"
    );
    assert!(
        sdl.contains("a: AnInput = {}"),
        "empty object default should survive:\n{sdl}"
    );
    assert!(
        sdl.contains("extension: Boolean! = false"),
        "spec directive argument default should survive:\n{sdl}"
    );
}

/// Regression test: an include filter used to mask every field of the introspection types,
/// which cascaded into masking `__Schema` itself and made the resulting supergraph fail to
/// produce an API schema with `Cannot remove reserved object field "Query.__schema"`.
#[test]
fn include_filtering_leaves_introspection_types_alone() {
    let supergraph = filter(TAGGED_SUPERGRAPH, &["public"], &[], false);

    for name in [
        "__Schema",
        "__Type",
        "__Field",
        "__Directive",
        "__EnumValue",
    ] {
        let ty = supergraph
            .schema()
            .schema()
            .types
            .get(name)
            .unwrap_or_else(|| panic!("{name} should be present"));
        assert!(
            !ty.directives().has("inaccessible"),
            "{name} should not be masked",
        );
    }

    // The failure this guards against only surfaced here, not in the serialized SDL:
    // apollo-compiler omits built-in types when printing.
    api_schema(&supergraph);
}

#[test]
fn include_filter_keeps_only_tagged_elements() {
    let supergraph = filter(TAGGED_SUPERGRAPH, &["public"], &[], false);
    insta::assert_snapshot!(api_schema(&supergraph));
}

#[test]
fn exclude_filter_drops_tagged_elements() {
    let supergraph = filter(TAGGED_SUPERGRAPH, &[], &["internal"], false);
    insta::assert_snapshot!(api_schema(&supergraph));
}

/// `Orphan` is referenced only from `Secret`, which the exclude filter masks, so it is
/// unreachable and gets masked in turn.
#[test]
fn hide_unreachable_types_masks_types_behind_a_masked_type() {
    let supergraph = filter(TAGGED_SUPERGRAPH, &[], &["internal"], true);
    insta::assert_snapshot!(api_schema(&supergraph));
}

/// Without `hide_unreachable_types`, the same `Orphan` stays in the API schema.
#[test]
fn keeps_unreachable_types_by_default() {
    let supergraph = filter(TAGGED_SUPERGRAPH, &[], &["internal"], false);
    let sdl = api_schema(&supergraph);
    assert!(sdl.contains("type Orphan"), "Orphan should survive:\n{sdl}");
}

#[test]
fn rejects_overlapping_filters() {
    let error = ContractFilters::new(["a", "b"], ["b", "c"], false)
        .expect_err("overlapping filters should be rejected");
    assert!(matches!(error, TransformError::OverlappingFilters(_)));
    insta::assert_snapshot!(error.to_string(), @"Include and exclude filters cannot overlap: b");
    assert_eq!(error.code(), "INVALID_FILTER_CONFIGURATION");
}

#[test]
fn rejects_an_unparseable_supergraph() {
    let filters = ContractFilters::new(Vec::<String>::new(), Vec::<String>::new(), false).unwrap();
    let error =
        filter_schema("type Query { oops", &filters).expect_err("invalid SDL should be rejected");
    assert!(matches!(error, TransformError::InvalidSupergraph(_)));
    assert_eq!(error.code(), "INVALID_SUPERGRAPH");
}

/// A malformed `@tag` definition is rejected by the core spec machinery in step 1. This
/// used to pass silently: `tag_values` matches on an argument literally named `name`, so a
/// renamed argument made every element look untagged and an include filter masked the whole
/// schema, root type included.
#[test]
fn rejects_a_tag_definition_whose_argument_is_renamed() {
    let sdl = TAGGED_SUPERGRAPH.replace(
        "@tag(name: String!) repeatable",
        "@tag(label: String!) repeatable",
    );
    let filters = ContractFilters::new(["public"], Vec::<String>::new(), false).unwrap();
    let error = filter_schema(&sdl, &filters).expect_err("a renamed argument should be rejected");

    assert_eq!(error.step(), FilterStep::AddDirectiveDefinitions);
    let message = error.to_string();
    assert!(
        message.contains(r#"missing required argument "name""#)
            && message.contains(r#"unknown/unsupported argument "label""#),
        "{message}"
    );
}

/// Old composition versions emitted `@tag` definitions narrower than the spec they link.
/// Those stay acceptable -- the core check only rejects locations *beyond* the spec.
#[test]
fn accepts_a_tag_definition_narrower_than_its_spec() {
    let sdl = TAGGED_SUPERGRAPH.replace(
        "directive @tag(name: String!) repeatable on FIELD_DEFINITION | INTERFACE | OBJECT | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA",
        "directive @tag(name: String!) repeatable on FIELD_DEFINITION | INTERFACE | OBJECT | UNION",
    );
    let supergraph = filter(&sdl, &["public"], &[], false);
    insta::assert_snapshot!(api_schema(&supergraph));
}

/// An SDL extension can tag a built-in scalar, which puts it among the `@tag` referencers
/// that exclusion starts from. Built-in types must still never be masked.
///
/// `Int` has to be used somewhere: validation drops built-in scalars nothing references.
#[test]
fn never_masks_a_tagged_built_in_scalar() {
    let sdl = format!(
        "{TAGGED_SUPERGRAPH}\nextend type Query {{ count: Int }}\nextend scalar Int @tag(name: \"private\")\n"
    );
    let supergraph = filter(&sdl, &[], &["private"], false);

    let int = &supergraph.schema().schema().types["Int"];
    assert!(
        !int.directives().has("inaccessible"),
        "built-in `Int` should stay accessible, got: {int}"
    );
}
