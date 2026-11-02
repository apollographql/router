//! Test harness shared by the per-step unit tests.
//!
//! Ported from the TypeScript suite's `compositionSchemaElements.ts` and
//! `SchemaTester.ts`. The 39 typed `expect*` methods of the original collapse into
//! [`Tester::inaccessible`], [`Tester::accessible`] and [`Tester::tags`], which address
//! elements by GraphQL schema coordinate:
//!
//! | Coordinate          | Element                  |
//! |---------------------|--------------------------|
//! | `Type`              | a named type             |
//! | `Type.field`        | a field or enum value    |
//! | `Type.field(arg:)`  | a field argument         |
//! | `@directive(arg:)`  | a directive argument     |

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::schema::DirectiveList;
use apollo_compiler::schema::ExtendedType;

use super::ContractFilters;
use super::helpers::FilterDirectiveMetadata;
use super::helpers::tag_values;
use crate::error::FederationError;
use crate::schema::FederationSchema;

/// Which parts of the standard composition preamble to emit.
///
/// Mirrors the options bag of the TypeScript `compositionSchemaElements`. [`Default`]
/// matches its defaults: every spec linked and every spec element defined, except the
/// dummy `bar` spec.
pub(crate) struct Preamble {
    pub(crate) use_schema: bool,
    pub(crate) use_core_spec: bool,
    pub(crate) use_core_spec_elements: bool,
    pub(crate) use_join_spec: bool,
    pub(crate) use_join_spec_elements: bool,
    pub(crate) use_tag_spec: bool,
    pub(crate) use_tag_spec_elements: bool,
    pub(crate) use_inaccessible_spec: bool,
    pub(crate) use_inaccessible_spec_elements: bool,
    /// A dummy spec linked as `bar` (really `foo`, renamed via `as:`), used by the
    /// tests that cover core schema element behaviour.
    pub(crate) use_bar_spec: bool,
}

impl Default for Preamble {
    fn default() -> Self {
        Self {
            use_schema: true,
            use_core_spec: true,
            use_core_spec_elements: true,
            use_join_spec: true,
            use_join_spec_elements: true,
            use_tag_spec: true,
            use_tag_spec_elements: true,
            use_inaccessible_spec: true,
            use_inaccessible_spec_elements: true,
            use_bar_spec: false,
        }
    }
}

const CORE_SPEC_ELEMENTS: &str = r#"
directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

enum link__Purpose {
  SECURITY
  EXECUTION
}

scalar link__Import
"#;

const JOIN_SPEC_ELEMENTS: &str = r#"
directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

directive @join__graph(name: String!, url: String!) on ENUM_VALUE

directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

enum join__Graph {
  TEST @join__graph(name: "test", url: "undefined")
}

scalar join__FieldSet
"#;

const TAG_SPEC_ELEMENTS: &str = r#"
"Composition @tag definition"
directive @tag(name: String!) repeatable on FIELD_DEFINITION | INTERFACE | OBJECT | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA
"#;

const INACCESSIBLE_SPEC_ELEMENTS: &str = r#"
"Composition @inaccessible definition"
directive @inaccessible on FIELD_DEFINITION | INTERFACE | OBJECT | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION
"#;

impl Preamble {
    /// The standard schema elements composition adds to a supergraph.
    pub(crate) fn to_sdl(&self) -> String {
        let mut sdl = String::new();
        let mut push = |enabled: bool, chunk: &str| {
            if enabled {
                sdl.push_str(chunk);
                sdl.push('\n');
            }
        };

        push(self.use_core_spec_elements, CORE_SPEC_ELEMENTS);
        push(self.use_join_spec_elements, JOIN_SPEC_ELEMENTS);
        push(self.use_tag_spec_elements, TAG_SPEC_ELEMENTS);
        push(
            self.use_inaccessible_spec_elements,
            INACCESSIBLE_SPEC_ELEMENTS,
        );
        push(self.use_schema, "schema {\n  query: Query\n}");
        push(
            self.use_core_spec,
            r#"extend schema @link(url: "https://specs.apollo.dev/link/v1.0")"#,
        );
        push(
            self.use_join_spec,
            r#"extend schema @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)"#,
        );
        push(
            self.use_tag_spec,
            r#"extend schema @link(url: "https://specs.apollo.dev/tag/v0.3")"#,
        );
        // `for: SECURITY` is deliberately left out: old Fed 2 composition versions
        // mistakenly forget it.
        push(
            self.use_inaccessible_spec,
            r#"extend schema @link(url: "https://specs.apollo.dev/inaccessible/v0.2")"#,
        );
        push(
            self.use_bar_spec,
            r#"extend schema @link(url: "http://localhost:8080/foo/v1.0", as: "bar")"#,
        );

        sdl
    }
}

/// Parse `sdl` on top of the default composition preamble.
pub(crate) fn supergraph(sdl: &str) -> Schema {
    supergraph_with(&Preamble::default(), sdl)
}

/// Parse `sdl` on top of `preamble`.
pub(crate) fn supergraph_with(preamble: &Preamble, sdl: &str) -> Schema {
    let sdl = format!("{}\n{sdl}", preamble.to_sdl());
    Schema::builder()
        .adopt_orphan_extensions()
        .parse(&sdl, "supergraph.graphql")
        .build()
        .unwrap_or_else(|e| panic!("test schema should parse: {}\n\n{sdl}", e.errors))
}

/// Tag filters, with unreachable type hiding off.
pub(crate) fn filters(include: &[&str], exclude: &[&str]) -> ContractFilters {
    ContractFilters::new(include.iter().copied(), exclude.iter().copied(), false)
        .expect("test filters should not overlap")
}

/// Tag filters, with unreachable type hiding on.
pub(crate) fn filters_hiding_unreachable(include: &[&str]) -> ContractFilters {
    ContractFilters::new(include.iter().copied(), std::iter::empty::<&str>(), true)
        .expect("test filters should not overlap")
}

/// Wrap a test schema for the steps that go through the spec machinery.
pub(crate) fn federation_schema(schema: Schema) -> FederationSchema {
    FederationSchema::new(schema).expect("test schema should build a FederationSchema")
}

/// Run `step` over `schema` the way the pipeline does -- through `FederationSchema`, with
/// metadata resolved from the schema itself -- and hand back the edited schema.
#[track_caller]
pub(crate) fn run_step(
    schema: Schema,
    step: impl FnOnce(&mut FederationSchema, &FilterDirectiveMetadata) -> Result<(), FederationError>,
) -> Schema {
    let mut schema = federation_schema(schema);
    let metadata = FilterDirectiveMetadata::from_schema(&schema);
    step(&mut schema, &metadata).expect("step should succeed");
    schema.into_inner()
}

/// Resolve the `@tag`/`@inaccessible`/`@link` names for a test schema.
pub(crate) fn metadata(schema: &Schema) -> FilterDirectiveMetadata {
    FilterDirectiveMetadata::from_schema(&federation_schema(schema.clone()))
}

/// Asserts accessibility and tags of schema elements addressed by coordinate.
pub(crate) struct Tester<'a> {
    schema: &'a Schema,
    tag: Name,
    inaccessible: Name,
}

impl<'a> Tester<'a> {
    pub(crate) fn new(schema: &'a Schema) -> Self {
        let metadata = metadata(schema);
        Self {
            schema,
            tag: metadata.tag,
            inaccessible: metadata.inaccessible,
        }
    }

    /// Every coordinate exists and carries `@inaccessible`.
    #[track_caller]
    pub(crate) fn inaccessible(&self, coordinates: &[&str]) {
        for coordinate in coordinates {
            assert!(
                self.directives(coordinate).has(&self.inaccessible),
                "expected `{coordinate}` to be @inaccessible",
            );
        }
    }

    /// Every coordinate exists and does not carry `@inaccessible`.
    #[track_caller]
    pub(crate) fn accessible(&self, coordinates: &[&str]) {
        for coordinate in coordinates {
            assert!(
                !self.directives(coordinate).has(&self.inaccessible),
                "expected `{coordinate}` to be accessible",
            );
        }
    }

    /// The coordinate carries exactly `expected` as its `@tag` names.
    ///
    /// Compared as a set, like the TypeScript `SchemaTester.expectTags`: tag *order* is
    /// pinned separately by the full-SDL snapshots in [`super::tagging`].
    #[track_caller]
    pub(crate) fn tags(&self, coordinate: &str, expected: &[&str]) {
        let mut actual: Vec<String> =
            tag_values(self.directives(coordinate).get_all(&self.tag).map(|d| &**d))
                .into_iter()
                .collect();
        let mut expected: Vec<String> = expected.iter().map(|t| t.to_string()).collect();
        let deduped = expected
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len();
        assert_eq!(
            deduped,
            expected.len(),
            "`{coordinate}` expectation has duplicates"
        );

        actual.sort();
        expected.sort();
        assert_eq!(actual, expected, "unexpected tags on `{coordinate}`");
    }

    /// The directive definition, printed. Includes its description, so this doubles as
    /// a check that a pre-existing definition was not clobbered.
    #[track_caller]
    pub(crate) fn directive(&self, name: &str) -> String {
        self.schema
            .directive_definitions
            .get(name)
            .unwrap_or_else(|| panic!("`@{name}` should be defined"))
            .to_string()
            .trim()
            .to_string()
    }

    /// The schema definition's `@link` application for `url`, printed.
    ///
    /// Covers `for:`, `as:` and `import:` in one assertion, standing in for the
    /// TypeScript suite's `getCoreFeatures(...).inaccessibleSpec.feature` checks.
    #[track_caller]
    pub(crate) fn link(&self, url: &str) -> String {
        self.schema
            .schema_definition
            .directives
            .iter()
            .find(|directive| {
                directive
                    .arguments
                    .iter()
                    .any(|arg| arg.name == "url" && arg.value.as_str() == Some(url))
            })
            .unwrap_or_else(|| panic!("schema should link `{url}`"))
            .to_string()
    }

    #[track_caller]
    fn directives(&self, coordinate: &str) -> DirectiveList {
        self.lookup(coordinate)
            .unwrap_or_else(|| panic!("`{coordinate}` should exist in the schema"))
    }

    /// Resolve a coordinate to its directives.
    fn lookup(&self, coordinate: &str) -> Option<DirectiveList> {
        // `@directive(arg:)`
        if let Some(rest) = coordinate.strip_prefix('@') {
            let (directive_name, arg_name) = split_argument(rest)?;
            let definition = self.schema.directive_definitions.get(directive_name)?;
            let argument = definition.argument_by_name(arg_name)?;
            return Some(argument.directives.clone());
        }

        let Some((type_name, member)) = coordinate.split_once('.') else {
            let ty = self.schema.types.get(coordinate)?;
            return Some(ty.directives().iter().cloned().collect());
        };
        let ty = self.schema.types.get(type_name)?;

        // `Type.field(arg:)`
        if let Some((field_name, arg_name)) = split_argument(member) {
            let field = match ty {
                ExtendedType::Object(obj) => obj.fields.get(field_name)?,
                ExtendedType::Interface(iface) => iface.fields.get(field_name)?,
                _ => return None,
            };
            let argument = field.argument_by_name(arg_name)?;
            return Some(argument.directives.clone());
        }

        // `Type.field`, covering enum values and input object fields
        match ty {
            ExtendedType::Object(obj) => {
                Some(obj.fields.get(member)?.directives.iter().cloned().collect())
            }
            ExtendedType::Interface(iface) => Some(
                iface
                    .fields
                    .get(member)?
                    .directives
                    .iter()
                    .cloned()
                    .collect(),
            ),
            ExtendedType::InputObject(input) => Some(input.fields.get(member)?.directives.clone()),
            ExtendedType::Enum(enm) => Some(enm.values.get(member)?.directives.clone()),
            _ => None,
        }
    }
}

/// Split `name(arg:)` into `("name", "arg")`.
fn split_argument(coordinate: &str) -> Option<(&str, &str)> {
    let (name, rest) = coordinate.split_once('(')?;
    let arg = rest.strip_suffix(":)")?;
    Some((name, arg))
}
