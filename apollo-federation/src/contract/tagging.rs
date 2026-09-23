use apollo_compiler::Name;
use apollo_compiler::collections::IndexSet;

use super::ContractFilters;
use super::helpers::FilterDirectiveMetadata;
use super::helpers::applied_tags;
use super::helpers::filterable_types;
use super::helpers::is_built_in_directive;
use super::helpers::is_filterable_type;
use super::helpers::mark_inaccessible;
use super::helpers::tag_directive;
use crate::error::FederationError;
use crate::merger::merge_argument::HasArguments;
use crate::schema::FederationSchema;
use crate::schema::directive_location::DirectiveLocationExt;
use crate::schema::position::DirectiveTargetPosition;
use crate::schema::position::ObjectOrInterfaceTypeDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;

/// Step 4: Tag inheriting.
///
/// Propagates @tag directives from parent elements to descendants:
/// - Object/Interface type -> its fields -> its arguments
/// - Input object type -> its fields
/// - Enum type -> its values
///
/// Only elements that already carry a `@tag` can pass one down, so the walk starts from the
/// `@tag` referencers rather than every type in the schema. Inserting through the position
/// API keeps those referencers current, which the field -> argument pass relies on.
pub(crate) fn step4_tag_inheriting(
    schema: &mut FederationSchema,
    metadata: &FilterDirectiveMetadata,
) -> Result<(), FederationError> {
    // Cloned because the inserts below record new `@tag` applications in the referencers.
    let tagged = schema.referencers().get_directive(&metadata.tag).clone();

    let composite_types = tagged
        .object_types
        .into_iter()
        .map(ObjectOrInterfaceTypeDefinitionPosition::from)
        .chain(
            tagged
                .interface_types
                .into_iter()
                .map(ObjectOrInterfaceTypeDefinitionPosition::from),
        );
    for type_ in composite_types {
        let tags = applied_tags(&type_, schema, metadata);
        let fields: Vec<_> = type_.fields(schema.schema())?.collect();
        for field in fields {
            inherit_tags(schema, field.into(), &tags, metadata)?;
        }
    }

    for enum_type in tagged.enum_types {
        let tags = applied_tags(&enum_type, schema, metadata);
        let values: Vec<_> = enum_type
            .get(schema.schema())?
            .values
            .keys()
            .map(|name| enum_type.value(name.clone()))
            .collect();
        for value in values {
            inherit_tags(schema, value.into(), &tags, metadata)?;
        }
    }

    for input_type in tagged.input_object_types {
        let tags = applied_tags(&input_type, schema, metadata);
        let fields: Vec<_> = input_type
            .get(schema.schema())?
            .fields
            .keys()
            .map(|name| input_type.field(name.clone()))
            .collect();
        for field in fields {
            inherit_tags(schema, field.into(), &tags, metadata)?;
        }
    }

    // Re-read after the type -> field pass, so a field passes down the tags it inherited as
    // well as its own, and a field that was untagged until now is included.
    let tagged_fields: Vec<_> = schema
        .referencers()
        .get_directive(&metadata.tag)
        .object_or_interface_fields()
        .collect();
    for field in tagged_fields {
        let tags = applied_tags(&field, schema, metadata);
        let arguments: Vec<_> = field
            .get_arguments(schema)?
            .iter()
            .map(|arg| field.argument_position(arg.name.clone()))
            .collect();
        for argument in arguments {
            inherit_tags(schema, argument.into(), &tags, metadata)?;
        }
    }

    Ok(())
}

/// Apply each of `tags` that `child` does not already carry, after its own tags.
fn inherit_tags(
    schema: &mut FederationSchema,
    child: DirectiveTargetPosition,
    tags: &IndexSet<String>,
    metadata: &FilterDirectiveMetadata,
) -> Result<(), FederationError> {
    let own = applied_tags(&child, schema, metadata);
    for value in tags.iter().filter(|value| !own.contains(*value)) {
        child.insert_directive(schema, tag_directive(&metadata.tag, value))?;
    }
    Ok(())
}

/// Step 5: Tag matching (core filtering).
///
/// Marks an element `@inaccessible` when it carries an excluded tag, or when include filters
/// are set and it carries no included tag. Include filters only apply to elements that cannot
/// have children -- a type or field is left for steps 6-11 to mask once its children are gone.
///
/// Only a tagged element can carry an excluded tag, so exclusion starts from the `@tag`
/// referencers. Inclusion has to visit every leaf, since an untagged leaf is exactly what it
/// masks.
pub(crate) fn step5_tag_matching(
    schema: &mut FederationSchema,
    metadata: &FilterDirectiveMetadata,
    filters: &ContractFilters,
) -> Result<(), FederationError> {
    if !filters.exclude.is_empty() {
        let tagged: Vec<_> = schema
            .referencers()
            .get_directive(&metadata.tag)
            .iter()
            .filter(|position| is_matchable(schema, metadata, position))
            .collect();
        for position in tagged {
            let tags = applied_tags(&position, schema, metadata);
            if tags.iter().any(|tag| filters.exclude.contains(tag)) {
                mark_inaccessible(schema, position, metadata)?;
            }
        }
    }

    if !filters.include.is_empty() {
        for position in leaf_elements(schema, metadata)? {
            let tags = applied_tags(&position, schema, metadata);
            if !tags.iter().any(|tag| filters.include.contains(tag)) {
                mark_inaccessible(schema, position, metadata)?;
            }
        }
    }

    Ok(())
}

/// Whether a `@tag`ged `position` is one tag matching may mask.
///
/// `@tag` is valid on more locations than `@inaccessible`, and on elements filtering must
/// leave alone:
/// - the schema definition, where `@inaccessible` is not a valid location;
/// - built-in and core spec types, and anything inside them (see [`is_filterable_type`]);
/// - arguments of directives [`is_matchable_directive`] rules out.
fn is_matchable(
    schema: &FederationSchema,
    metadata: &FilterDirectiveMetadata,
    position: &DirectiveTargetPosition,
) -> bool {
    let type_name = match position {
        DirectiveTargetPosition::Schema(_) => return false,
        DirectiveTargetPosition::DirectiveArgument(argument) => {
            return is_matchable_directive(schema, metadata, &argument.directive_name);
        }
        DirectiveTargetPosition::ScalarType(p) => &p.type_name,
        DirectiveTargetPosition::ObjectType(p) => &p.type_name,
        DirectiveTargetPosition::ObjectField(p) => &p.type_name,
        DirectiveTargetPosition::ObjectFieldArgument(p) => &p.type_name,
        DirectiveTargetPosition::InterfaceType(p) => &p.type_name,
        DirectiveTargetPosition::InterfaceField(p) => &p.type_name,
        DirectiveTargetPosition::InterfaceFieldArgument(p) => &p.type_name,
        DirectiveTargetPosition::UnionType(p) => &p.type_name,
        DirectiveTargetPosition::EnumType(p) => &p.type_name,
        DirectiveTargetPosition::EnumValue(p) => &p.type_name,
        DirectiveTargetPosition::InputObjectType(p) => &p.type_name,
        DirectiveTargetPosition::InputObjectField(p) => &p.type_name,
    };
    is_filterable_type(schema.schema(), metadata, type_name)
}

/// Whether tag matching applies to the arguments of directive `name`.
///
/// Skips built-in directives, core feature directives, and directives that have any
/// non-operation (type-system) location -- matching the JS behavior in tagMatching's
/// `MapperKind.DIRECTIVE` handler.
fn is_matchable_directive(
    schema: &FederationSchema,
    metadata: &FilterDirectiveMetadata,
    name: &Name,
) -> bool {
    !is_built_in_directive(schema.schema(), name)
        && !metadata.is_core_directive(name)
        && schema
            .schema()
            .directive_definitions
            .get(name)
            .is_some_and(|definition| {
                definition
                    .locations
                    .iter()
                    .all(|location| location.is_executable_location())
            })
}

/// Every matchable element that cannot have children: unions, scalars, enum values, input
/// object fields, field arguments and matchable directive arguments.
fn leaf_elements(
    schema: &FederationSchema,
    metadata: &FilterDirectiveMetadata,
) -> Result<Vec<DirectiveTargetPosition>, FederationError> {
    let mut leaves: Vec<DirectiveTargetPosition> = Vec::new();
    for type_ in filterable_types(schema, metadata) {
        match type_ {
            TypeDefinitionPosition::Union(union_type) => leaves.push(union_type.into()),
            TypeDefinitionPosition::Scalar(scalar) => leaves.push(scalar.into()),
            TypeDefinitionPosition::Enum(enum_type) => leaves.extend(
                enum_type
                    .get(schema.schema())?
                    .values
                    .keys()
                    .map(|name| enum_type.value(name.clone()).into()),
            ),
            TypeDefinitionPosition::InputObject(input_type) => leaves.extend(
                input_type
                    .get(schema.schema())?
                    .fields
                    .keys()
                    .map(|name| input_type.field(name.clone()).into()),
            ),
            TypeDefinitionPosition::Object(_) | TypeDefinitionPosition::Interface(_) => {
                let type_ = ObjectOrInterfaceTypeDefinitionPosition::try_from(type_)?;
                for field in type_.fields(schema.schema())? {
                    for argument in field.get_arguments(schema)? {
                        leaves.push(field.argument_position(argument.name.clone()).into());
                    }
                }
            }
        }
    }
    for directive in schema.get_directive_definitions() {
        if !is_matchable_directive(schema, metadata, &directive.directive_name) {
            continue;
        }
        for argument in &directive.get(schema.schema())?.arguments {
            leaves.push(directive.argument(argument.name.clone()).into());
        }
    }
    Ok(leaves)
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;

    use super::step4_tag_inheriting;

    const TAG_DEFINITION: &str = r#"
        directive @tag(name: String!) repeatable on
            | FIELD_DEFINITION
            | OBJECT
            | INTERFACE
            | UNION
            | ARGUMENT_DEFINITION
            | SCALAR
            | ENUM
            | ENUM_VALUE
            | INPUT_OBJECT
            | INPUT_FIELD_DEFINITION
    "#;

    /// Parse `sdl` with the `@tag` definition prepended, run step 4, and
    /// re-serialize `type_name` so assertions read as plain SDL.
    ///
    /// Inherited tags are appended after the element's own tags, in the order
    /// the parent declares them -- see the `IndexSet` in [`get_tags`].
    fn inherit(sdl: &str, type_name: &str) -> String {
        let schema = Schema::builder()
            .adopt_orphan_extensions()
            .parse(format!("{TAG_DEFINITION}\n{sdl}"), "test.graphql")
            .build()
            .expect("test schema should parse");
        crate::contract::testing::run_step(schema, step4_tag_inheriting)
            .types
            .get(type_name)
            .expect("type should exist")
            .to_string()
    }

    /// 4.1: object -> field -> argument inheritance accumulates tags at each hop.
    #[test]
    fn inherits_object_tags_to_fields_and_arguments() {
        let filtered = inherit(
            r#"
            type Query @tag(name: "A") @tag(name: "B") @tag(name: "C") {
              foo(arg: String @tag(name: "C") @tag(name: "D") @tag(name: "E")): String
                @tag(name: "B") @tag(name: "D")
            }
            "#,
            "Query",
        );

        assert_eq!(
            filtered,
            r#"type Query @tag(name: "A") @tag(name: "B") @tag(name: "C") {
  foo(
    arg: String @tag(name: "C") @tag(name: "D") @tag(name: "E") @tag(name: "B") @tag(name: "A"),
  ): String @tag(name: "B") @tag(name: "D") @tag(name: "A") @tag(name: "C")
}
"#
        );
    }

    /// 4.2: interfaces follow the same rules as objects.
    #[test]
    fn inherits_interface_tags_to_fields_and_arguments() {
        let filtered = inherit(
            r#"
            interface Node @tag(name: "A") @tag(name: "B") {
              foo(arg: String @tag(name: "C") @tag(name: "D")): String
                @tag(name: "B") @tag(name: "C")
            }

            type Query { node: Node }
            "#,
            "Node",
        );

        assert_eq!(
            filtered,
            r#"interface Node @tag(name: "A") @tag(name: "B") {
  foo(
    arg: String @tag(name: "C") @tag(name: "D") @tag(name: "B") @tag(name: "A"),
  ): String @tag(name: "B") @tag(name: "C") @tag(name: "A")
}
"#
        );
    }

    /// 4.3: enum -> value inheritance.
    #[test]
    fn inherits_enum_tags_to_values() {
        let filtered = inherit(
            r#"
            enum Color @tag(name: "A") @tag(name: "B") {
              RED @tag(name: "B") @tag(name: "C")
            }

            type Query { color: Color }
            "#,
            "Color",
        );

        assert_eq!(
            filtered,
            r#"enum Color @tag(name: "A") @tag(name: "B") {
  RED @tag(name: "B") @tag(name: "C") @tag(name: "A")
}
"#
        );
    }

    /// 4.4: input object -> field inheritance.
    #[test]
    fn inherits_input_object_tags_to_fields() {
        let filtered = inherit(
            r#"
            input Filter @tag(name: "A") @tag(name: "B") {
              term: String @tag(name: "B") @tag(name: "C")
            }

            type Query { search(filter: Filter): String }
            "#,
            "Filter",
        );

        assert_eq!(
            filtered,
            r#"input Filter @tag(name: "A") @tag(name: "B") {
  term: String @tag(name: "B") @tag(name: "C") @tag(name: "A")
}
"#
        );
    }

    /// 4.5: a tagged field on an *untagged* object still propagates to its arguments.
    #[test]
    fn inherits_field_tags_to_arguments_when_object_is_untagged() {
        let filtered = inherit(
            r#"
            type Query {
              foo(arg: String @tag(name: "B")): String @tag(name: "A")
            }
            "#,
            "Query",
        );

        assert_eq!(
            filtered,
            r#"type Query {
  foo(
    arg: String @tag(name: "B") @tag(name: "A"),
  ): String @tag(name: "A")
}
"#
        );
    }

    /// 4.6: same as 4.5 for interfaces.
    #[test]
    fn inherits_field_tags_to_arguments_when_interface_is_untagged() {
        let filtered = inherit(
            r#"
            interface Node {
              foo(arg: String @tag(name: "B")): String @tag(name: "A")
            }

            type Query { node: Node }
            "#,
            "Node",
        );

        assert_eq!(
            filtered,
            r#"interface Node {
  foo(
    arg: String @tag(name: "B") @tag(name: "A"),
  ): String @tag(name: "A")
}
"#
        );
    }

    /// 4.7: with nothing to inherit, tags are left exactly as authored.
    #[test]
    fn leaves_tags_untouched_when_there_is_nothing_to_inherit() {
        let filtered = inherit(
            r#"
            type Query {
              foo(arg: String @tag(name: "B")): String
            }
            "#,
            "Query",
        );

        assert_eq!(
            filtered,
            r#"type Query {
  foo(
    arg: String @tag(name: "B"),
  ): String
}
"#
        );
    }

    /// 4.8: inheriting a tag an element already carries does not duplicate it.
    #[test]
    fn does_not_duplicate_already_present_tags() {
        let filtered = inherit(
            r#"
            type Query @tag(name: "A") {
              foo(arg: String @tag(name: "A")): String @tag(name: "A")
            }
            "#,
            "Query",
        );

        assert_eq!(
            filtered,
            r#"type Query @tag(name: "A") {
  foo(
    arg: String @tag(name: "A"),
  ): String @tag(name: "A")
}
"#
        );
    }
}

#[cfg(test)]
mod tag_inheriting_tests {
    use apollo_compiler::Schema;

    use super::*;
    use crate::contract::testing::Tester;
    use crate::contract::testing::run_step;
    use crate::contract::testing::supergraph;

    /// Run step 4 over `sdl` layered on the composition preamble.
    fn tag_inheriting(sdl: &str) -> Schema {
        run_step(supergraph(sdl), step4_tag_inheriting)
    }

    #[test]
    fn object_fields_and_arguments_inherit_tags() {
        let schema = tag_inheriting(
            r#"
            type Query { foo: String }

            type Object
              @tag(name: "object")
              @tag(name: "object_field")
              @tag(name: "object_argument") {
              field(
                argument: String!
                  @tag(name: "object_argument")
                  @tag(name: "field_argument")
                  @tag(name: "argument")
              ): String!
                @tag(name: "object_field")
                @tag(name: "field")
                @tag(name: "field_argument")
            }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.tags("Object", &["object", "object_field", "object_argument"]);
        tester.tags(
            "Object.field",
            &[
                "object",
                "field",
                "object_field",
                "object_argument",
                "field_argument",
            ],
        );
        tester.tags(
            "Object.field(argument:)",
            &[
                "object",
                "field",
                "argument",
                "object_field",
                "object_argument",
                "field_argument",
            ],
        );
    }

    #[test]
    fn interface_fields_and_arguments_inherit_tags() {
        let schema = tag_inheriting(
            r#"
            type Query { foo: String }

            interface Interface
              @tag(name: "interface")
              @tag(name: "interface_field")
              @tag(name: "interface_argument") {
              field(
                argument: String!
                  @tag(name: "interface_argument")
                  @tag(name: "field_argument")
                  @tag(name: "argument")
              ): String!
                @tag(name: "interface_field")
                @tag(name: "field")
                @tag(name: "field_argument")
            }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.tags(
            "Interface",
            &["interface", "interface_field", "interface_argument"],
        );
        tester.tags(
            "Interface.field",
            &[
                "interface",
                "field",
                "interface_field",
                "interface_argument",
                "field_argument",
            ],
        );
        tester.tags(
            "Interface.field(argument:)",
            &[
                "interface",
                "field",
                "argument",
                "interface_field",
                "interface_argument",
                "field_argument",
            ],
        );
    }

    #[test]
    fn enum_values_inherit_tags() {
        let schema = tag_inheriting(
            r#"
            type Query { foo: String }

            enum Enum @tag(name: "enum") @tag(name: "enum_value") {
              VALUE @tag(name: "enum_value") @tag(name: "value")
            }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.tags("Enum", &["enum", "enum_value"]);
        tester.tags("Enum.VALUE", &["enum", "value", "enum_value"]);
    }

    #[test]
    fn input_object_fields_inherit_tags() {
        let schema = tag_inheriting(
            r#"
            type Query { foo: String }

            input Input @tag(name: "input") @tag(name: "input_field") {
              field: String! @tag(name: "input_field") @tag(name: "field")
            }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.tags("Input", &["input", "input_field"]);
        tester.tags("Input.field", &["input", "field", "input_field"]);
    }
}

#[cfg(test)]
mod tag_matching_tests {
    use apollo_compiler::Schema;

    use super::*;
    use crate::contract::testing::Preamble;
    use crate::contract::testing::Tester;
    use crate::contract::testing::filters;
    use crate::contract::testing::run_step;
    use crate::contract::testing::supergraph_with;

    /// A schema containing every element kind that tag v0.2 can tag, plus every kind
    /// that tag matching must ignore. Written as though it had already been through
    /// tag inheritance.
    ///
    /// `tags` are applied to each element marked "tagged".
    fn test_schema(tags: &[&str]) -> String {
        let d = tags
            .iter()
            .map(|tag| format!(r#"@tag(name: "{tag}")"#))
            .collect::<Vec<_>>()
            .join(" ");

        format!(
            r#"
            # Root operation type tagged at argument
            type Query {{
              field(taggedArgument: String {d}, untaggedArgument: String): String
            }}

            # Root operation type tagged at field
            type Mutation {{
              taggedField(argument: String {d}): String {d}
              untaggedField(argument: String): String
            }}

            type TaggedObject implements TaggedInterface {d} {{
              field(argument: String {d}): String {d}
            }}
            type TaggedObjectField implements TaggedInterfaceField {{
              field(argument: String {d}): String {d}
            }}
            type TaggedObjectFieldArgument implements TaggedInterfaceField {{
              field(argument: String {d}): String
            }}
            type UntaggedObject implements UntaggedInterface {{
              field(argument: String): String
            }}

            interface TaggedInterface {d} {{
              field(argument: String {d}): String {d}
            }}
            interface TaggedInterfaceField {{
              field(argument: String {d}): String {d}
            }}
            interface TaggedInterfaceFieldArgument {{
              field(argument: String {d}): String
            }}
            interface UntaggedInterface {{
              field(argument: String): String
            }}

            union TaggedUnion {d} = TaggedObject | TaggedObjectField
            union UntaggedUnion = UntaggedObject | TaggedObject

            input TaggedInput {d} {{ field: String {d} }}
            input TaggedInputField {{ field: String {d} }}
            input UntaggedInput {{ field: String }}

            enum TaggedEnum {d} {{ VALUE {d} }}
            enum TaggedEnumValue {{ VALUE {d} }}
            enum UntaggedEnum {{ VALUE }}

            scalar TaggedScalar {d}
            scalar UntaggedScalar

            directive @taggedDirectiveArgument(argument: String! {d}) on FIELD
            directive @untaggedDirectiveArgument(argument: String!) on QUERY

            # Non-operation directives and their descendants (should be ignored)
            directive @taggedNonOperationDirectiveArgument(argument: String! {d}) on OBJECT | FIELD
            directive @untaggedNonOperationDirectiveArgument(argument: String!) on ENUM

            # Built-in types/directive and their descendants (should be ignored)
            directive @skip(if: Boolean! {d}) on FIELD | FRAGMENT_SPREAD | INLINE_FRAGMENT
            directive @include(if: Boolean!) on FIELD | FRAGMENT_SPREAD | INLINE_FRAGMENT

            # Core feature elements and their descendants (should be ignored)
            type bar__TaggedObject {d} {{
              field(argument: String {d}): String {d}
            }}
            type bar__UntaggedObject {{ field(argument: String): String }}
            interface bar__TaggedInterface {d} {{
              field(argument: String {d}): String {d}
            }}
            interface bar__UntaggedInterface {{ field(argument: String): String }}
            union bar__TaggedUnion {d} = bar__TaggedObject
            union bar__UntaggedUnion = bar__UntaggedObject
            input bar__TaggedInput {d} {{ field: String {d} }}
            input bar__UntaggedInput {{ field: String }}
            enum bar__TaggedEnum {d} {{ VALUE {d} }}
            enum bar__UntaggedEnum {{ VALUE }}
            scalar bar__TaggedScalar {d}
            scalar bar__UntaggedScalar
            directive @bar__TaggedDirectiveArgument(argument: String {d}) on FIELD
            directive @bar__UntaggedDirectiveArgument(argument: String) on FIELD
            # Core feature root directive and its descendants (should be ignored)
            directive @bar(argument: String {d}) on FIELD
            "#
        )
    }

    /// Run step 5 over [`test_schema`] with the given filters.
    fn tag_matching(tags: &[&str], include: &[&str], exclude: &[&str]) -> Schema {
        let preamble = Preamble {
            use_bar_spec: true,
            ..Preamble::default()
        };
        let filters = filters(include, exclude);
        run_step(
            supergraph_with(&preamble, &test_schema(tags)),
            |schema, metadata| step5_tag_matching(schema, metadata, &filters),
        )
    }

    /// Elements that tag matching must never touch, whatever the filters.
    #[track_caller]
    fn expect_ignored_elements(tester: &Tester) {
        tester.accessible(&[
            // Non-operation directives and their descendants
            "@taggedNonOperationDirectiveArgument(argument:)",
            "@untaggedNonOperationDirectiveArgument(argument:)",
            // Built-in types/directive and their descendants
            "String",
            "ID",
            "@skip(if:)",
            "@include(if:)",
            // Core feature elements and their descendants
            "bar__TaggedObject",
            "bar__TaggedObject.field",
            "bar__TaggedObject.field(argument:)",
            "bar__UntaggedObject",
            "bar__UntaggedObject.field",
            "bar__UntaggedObject.field(argument:)",
            "bar__TaggedInterface",
            "bar__TaggedInterface.field",
            "bar__TaggedInterface.field(argument:)",
            "bar__UntaggedInterface",
            "bar__UntaggedInterface.field",
            "bar__UntaggedInterface.field(argument:)",
            "bar__TaggedUnion",
            "bar__UntaggedUnion",
            "bar__TaggedInput",
            "bar__TaggedInput.field",
            "bar__UntaggedInput",
            "bar__UntaggedInput.field",
            "bar__TaggedEnum",
            "bar__TaggedEnum.VALUE",
            "bar__UntaggedEnum",
            "bar__UntaggedEnum.VALUE",
            "bar__TaggedScalar",
            "bar__UntaggedScalar",
            "@bar__TaggedDirectiveArgument(argument:)",
            "@bar__UntaggedDirectiveArgument(argument:)",
            // Core feature root directive and its descendants
            "@bar(argument:)",
        ]);
    }

    #[test]
    fn marks_nothing_inaccessible_when_there_are_no_filters() {
        let schema = tag_matching(&["private"], &[], &[]);
        let tester = Tester::new(&schema);

        tester.accessible(&[
            "Query",
            "Query.field",
            "Query.field(taggedArgument:)",
            "Query.field(untaggedArgument:)",
            "Mutation",
            "Mutation.taggedField",
            "Mutation.taggedField(argument:)",
            "Mutation.untaggedField",
            "Mutation.untaggedField(argument:)",
            "TaggedObject",
            "TaggedObject.field",
            "TaggedObject.field(argument:)",
            "TaggedObjectField",
            "TaggedObjectField.field",
            "TaggedObjectField.field(argument:)",
            "TaggedObjectFieldArgument",
            "TaggedObjectFieldArgument.field",
            "TaggedObjectFieldArgument.field(argument:)",
            "UntaggedObject",
            "UntaggedObject.field",
            "UntaggedObject.field(argument:)",
            "TaggedInterface",
            "TaggedInterface.field",
            "TaggedInterface.field(argument:)",
            "TaggedInterfaceField",
            "TaggedInterfaceField.field",
            "TaggedInterfaceField.field(argument:)",
            "TaggedInterfaceFieldArgument",
            "TaggedInterfaceFieldArgument.field",
            "TaggedInterfaceFieldArgument.field(argument:)",
            "UntaggedInterface",
            "UntaggedInterface.field",
            "UntaggedInterface.field(argument:)",
            "TaggedUnion",
            "UntaggedUnion",
            "TaggedInput",
            "TaggedInput.field",
            "TaggedInputField",
            "TaggedInputField.field",
            "UntaggedInput",
            "UntaggedInput.field",
            "TaggedEnum",
            "TaggedEnum.VALUE",
            "TaggedEnumValue",
            "TaggedEnumValue.VALUE",
            "UntaggedEnum",
            "UntaggedEnum.VALUE",
            "TaggedScalar",
            "UntaggedScalar",
            "@taggedDirectiveArgument(argument:)",
            "@untaggedDirectiveArgument(argument:)",
        ]);
        expect_ignored_elements(&tester);
    }

    #[test]
    fn marks_excluded_elements_inaccessible() {
        let schema = tag_matching(&["private"], &[], &["private"]);
        let tester = Tester::new(&schema);

        tester.accessible(&[
            "Query",
            "Query.field",
            "Query.field(untaggedArgument:)",
            "Mutation",
            "Mutation.untaggedField",
            "Mutation.untaggedField(argument:)",
            "TaggedObjectField",
            "TaggedObjectFieldArgument",
            "TaggedObjectFieldArgument.field",
            "UntaggedObject",
            "UntaggedObject.field",
            "UntaggedObject.field(argument:)",
            "TaggedInterfaceField",
            "TaggedInterfaceFieldArgument",
            "TaggedInterfaceFieldArgument.field",
            "UntaggedInterface",
            "UntaggedInterface.field",
            "UntaggedInterface.field(argument:)",
            "UntaggedUnion",
            "TaggedInputField",
            "UntaggedInput",
            "UntaggedInput.field",
            "TaggedEnumValue",
            "UntaggedEnum",
            "UntaggedEnum.VALUE",
            "UntaggedScalar",
            "@untaggedDirectiveArgument(argument:)",
        ]);
        tester.inaccessible(&[
            "Query.field(taggedArgument:)",
            "Mutation.taggedField",
            "Mutation.taggedField(argument:)",
            "TaggedObject",
            "TaggedObject.field",
            "TaggedObject.field(argument:)",
            "TaggedObjectField.field",
            "TaggedObjectField.field(argument:)",
            "TaggedObjectFieldArgument.field(argument:)",
            "TaggedInterface",
            "TaggedInterface.field",
            "TaggedInterface.field(argument:)",
            "TaggedInterfaceField.field",
            "TaggedInterfaceField.field(argument:)",
            "TaggedInterfaceFieldArgument.field(argument:)",
            "TaggedUnion",
            "TaggedInput",
            "TaggedInput.field",
            "TaggedInputField.field",
            "TaggedEnum",
            "TaggedEnum.VALUE",
            "TaggedEnumValue.VALUE",
            "TaggedScalar",
            // Deviation from the TypeScript test, which opts out of directive
            // argument filtering via `ignoreDirectiveDefinitionArguments: true`.
            // Production passes `false`, which is what this port implements.
            "@taggedDirectiveArgument(argument:)",
        ]);
        expect_ignored_elements(&tester);
    }

    #[test]
    fn marks_non_included_elements_inaccessible_except_types_that_can_have_children() {
        let schema = tag_matching(&["public"], &["public"], &[]);
        let tester = Tester::new(&schema);

        tester.accessible(&[
            "Query",
            "Query.field",
            "Query.field(taggedArgument:)",
            "Mutation",
            "Mutation.taggedField",
            "Mutation.taggedField(argument:)",
            "Mutation.untaggedField",
            "TaggedObject",
            "TaggedObject.field",
            "TaggedObject.field(argument:)",
            "TaggedObjectField",
            "TaggedObjectField.field",
            "TaggedObjectField.field(argument:)",
            "TaggedObjectFieldArgument",
            "TaggedObjectFieldArgument.field",
            "TaggedObjectFieldArgument.field(argument:)",
            "UntaggedObject",
            "UntaggedObject.field",
            "TaggedInterface",
            "TaggedInterface.field",
            "TaggedInterface.field(argument:)",
            "TaggedInterfaceField",
            "TaggedInterfaceField.field",
            "TaggedInterfaceField.field(argument:)",
            "TaggedInterfaceFieldArgument",
            "TaggedInterfaceFieldArgument.field",
            "TaggedInterfaceFieldArgument.field(argument:)",
            "UntaggedInterface",
            "UntaggedInterface.field",
            "TaggedUnion",
            "TaggedInput",
            "TaggedInput.field",
            "TaggedInputField",
            "TaggedInputField.field",
            "UntaggedInput",
            "TaggedEnum",
            "TaggedEnum.VALUE",
            "TaggedEnumValue",
            "TaggedEnumValue.VALUE",
            "UntaggedEnum",
            "TaggedScalar",
            "@taggedDirectiveArgument(argument:)",
        ]);
        tester.inaccessible(&[
            "Query.field(untaggedArgument:)",
            "Mutation.untaggedField(argument:)",
            "UntaggedObject.field(argument:)",
            "UntaggedInterface.field(argument:)",
            "UntaggedUnion",
            "UntaggedInput.field",
            "UntaggedEnum.VALUE",
            "UntaggedScalar",
            // Deviation: see `marks_excluded_elements_inaccessible`.
            "@untaggedDirectiveArgument(argument:)",
        ]);
        expect_ignored_elements(&tester);
    }

    #[test]
    fn lets_excludes_override_includes() {
        let schema = tag_matching(&["private", "public"], &["public"], &["private"]);
        let tester = Tester::new(&schema);

        tester.accessible(&[
            "Query",
            "Query.field",
            "Mutation",
            "Mutation.untaggedField",
            "TaggedObjectField",
            "TaggedObjectFieldArgument",
            "TaggedObjectFieldArgument.field",
            "UntaggedObject",
            "UntaggedObject.field",
            "TaggedInterfaceField",
            "TaggedInterfaceFieldArgument",
            "TaggedInterfaceFieldArgument.field",
            "UntaggedInterface",
            "UntaggedInterface.field",
            "TaggedInputField",
            "UntaggedInput",
            "TaggedEnumValue",
            "UntaggedEnum",
        ]);
        tester.inaccessible(&[
            "Query.field(taggedArgument:)",
            "Query.field(untaggedArgument:)",
            "Mutation.taggedField",
            "Mutation.taggedField(argument:)",
            "Mutation.untaggedField(argument:)",
            "TaggedObject",
            "TaggedObject.field",
            "TaggedObject.field(argument:)",
            "TaggedObjectField.field",
            "TaggedObjectField.field(argument:)",
            "TaggedObjectFieldArgument.field(argument:)",
            "UntaggedObject.field(argument:)",
            "TaggedInterface",
            "TaggedInterface.field",
            "TaggedInterface.field(argument:)",
            "TaggedInterfaceField.field",
            "TaggedInterfaceField.field(argument:)",
            "TaggedInterfaceFieldArgument.field(argument:)",
            "UntaggedInterface.field(argument:)",
            "TaggedUnion",
            "UntaggedUnion",
            "TaggedInput",
            "TaggedInput.field",
            "TaggedInputField.field",
            "UntaggedInput.field",
            "TaggedEnum",
            "TaggedEnum.VALUE",
            "TaggedEnumValue.VALUE",
            "UntaggedEnum.VALUE",
            "TaggedScalar",
            "UntaggedScalar",
            // Deviation: see `marks_excluded_elements_inaccessible`.
            "@taggedDirectiveArgument(argument:)",
            "@untaggedDirectiveArgument(argument:)",
        ]);
        expect_ignored_elements(&tester);
    }
}
