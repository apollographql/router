use std::collections::HashSet;
use std::collections::VecDeque;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::FieldDefinition;

use super::ContractFilters;
use super::helpers::FilterDirectiveMetadata;
use super::helpers::applied_tags;
use super::helpers::filterable_types;
use super::helpers::mark_inaccessible;
use crate::error::FederationError;
use crate::schema::FederationSchema;
use crate::schema::directive_location::DirectiveLocationExt;
use crate::schema::position::TypeDefinitionPosition;

/// Step 12: Unreachable type masking.
///
/// Marks every type that cannot be reached from the schema's entry points @inaccessible. The
/// entry points are the root operation types, the argument types of operation directives, and,
/// with include filters, the types explicitly tagged with an included tag.
///
/// The walk stops at anything `@inaccessible`: a masked type, field or argument reaches
/// nothing, so a type reachable only through one is masked in turn. It does walk through
/// built-in and core spec types, which are only exempt from being masked themselves.
pub(crate) fn step12_unreachable_type_masking(
    schema: &mut FederationSchema,
    metadata: &FilterDirectiveMetadata,
    filters: &ContractFilters,
) -> Result<(), FederationError> {
    let reachable = reachable_types(schema, metadata, filters);

    let unreachable: Vec<_> = filterable_types(schema, metadata)
        .filter(|type_| !reachable.contains(type_.type_name()))
        .collect();
    for type_ in unreachable {
        mark_inaccessible(schema, type_, metadata)?;
    }
    Ok(())
}

/// Breadth-first walk from the entry points. A type counts as reachable once it is visited
/// and found accessible; an inaccessible type is visited but reaches nothing.
fn reachable_types(
    schema: &FederationSchema,
    metadata: &FilterDirectiveMetadata,
    filters: &ContractFilters,
) -> HashSet<Name> {
    let referencers = schema.referencers();
    let inaccessible = &metadata.inaccessible;
    let mut queue: VecDeque<Name> = VecDeque::new();

    for (_, root) in schema.schema().schema_definition.iter_root_operations() {
        queue.push_back(Name::clone(root));
    }

    // Operation directives can appear in any operation, so their argument types are reachable.
    for definition in schema.schema().directive_definitions.values() {
        if definition
            .locations
            .iter()
            .any(|location| location.is_executable_location())
        {
            queue.extend(
                definition
                    .arguments
                    .iter()
                    .filter(|arg| !arg.directives.has(inaccessible))
                    .map(|arg| arg.ty.inner_named_type().clone()),
            );
        }
    }

    // A type explicitly tagged with an included tag is reachable in its own right.
    if !filters.include.is_empty() {
        let tagged = referencers.get_directive(&metadata.tag);
        let tagged_types = (tagged
            .scalar_types
            .iter()
            .cloned()
            .map(TypeDefinitionPosition::from))
        .chain(tagged.object_types.iter().cloned().map(Into::into))
        .chain(tagged.interface_types.iter().cloned().map(Into::into))
        .chain(tagged.union_types.iter().cloned().map(Into::into))
        .chain(tagged.enum_types.iter().cloned().map(Into::into))
        .chain(tagged.input_object_types.iter().cloned().map(Into::into));
        for type_ in tagged_types {
            if applied_tags(&type_, schema, metadata)
                .iter()
                .any(|tag| filters.include.contains(tag))
            {
                queue.push_back(type_.type_name().clone());
            }
        }
    }

    let mut reachable = HashSet::new();
    while let Some(type_name) = queue.pop_front() {
        let Some(type_) = schema.schema().types.get(&type_name) else {
            continue;
        };
        if type_.directives().has(inaccessible) || !reachable.insert(type_name.clone()) {
            continue;
        }

        match type_ {
            ExtendedType::Object(object) => {
                push_field_types(&mut queue, &object.fields, inaccessible);
                queue.extend(object.implements_interfaces.iter().map(|i| Name::clone(i)));
                // Reaching an object also reaches the unions it is a member of.
                if let Some(object) = referencers.object_types.get(&type_name) {
                    queue.extend(object.union_types.iter().map(|u| u.type_name.clone()));
                }
            }
            ExtendedType::Interface(interface) => {
                push_field_types(&mut queue, &interface.fields, inaccessible);
                // PORT NOTE: the JS traversal does not follow an interface to the interfaces it
                // implements, so a super-interface is only reached through an implementing
                // object. Following it here keeps interfaces consistent with objects, and only
                // differs when the sub-interface has no reachable implementing object.
                queue.extend(
                    interface
                        .implements_interfaces
                        .iter()
                        .map(|i| Name::clone(i)),
                );
                if let Some(interface) = referencers.interface_types.get(&type_name) {
                    queue.extend(interface.object_types.iter().map(|o| o.type_name.clone()));
                }
            }
            ExtendedType::Union(union_type) => {
                queue.extend(union_type.members.iter().map(|m| Name::clone(m)));
            }
            ExtendedType::InputObject(input) => {
                queue.extend(
                    input
                        .fields
                        .values()
                        .filter(|field| !field.directives.has(inaccessible))
                        .map(|field| field.ty.inner_named_type().clone()),
                );
            }
            ExtendedType::Enum(_) | ExtendedType::Scalar(_) => {}
        }
    }

    reachable
}

/// Queue the return and argument types of every accessible field.
fn push_field_types(
    queue: &mut VecDeque<Name>,
    fields: &IndexMap<Name, Node<FieldDefinition>>,
    inaccessible: &Name,
) {
    for field in fields.values().filter(|f| !f.directives.has(inaccessible)) {
        queue.push_back(field.ty.inner_named_type().clone());
        queue.extend(
            field
                .arguments
                .iter()
                .filter(|arg| !arg.directives.has(inaccessible))
                .map(|arg| arg.ty.inner_named_type().clone()),
        );
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;

    use super::*;
    use crate::contract::testing::Preamble;
    use crate::contract::testing::Tester;
    use crate::contract::testing::filters_hiding_unreachable;
    use crate::contract::testing::run_step;
    use crate::contract::testing::supergraph;
    use crate::contract::testing::supergraph_with;

    /// Run step 12 over `sdl` with the given include filters.
    fn unreachable_type_masking(sdl: &str, include: &[&str]) -> Schema {
        let filters = filters_hiding_unreachable(include);
        run_step(supergraph(sdl), |schema, metadata| {
            step12_unreachable_type_masking(schema, metadata, &filters)
        })
    }

    #[test]
    fn masks_unreachable_object_types() {
        let schema = unreachable_type_masking(
            r#"
            type Query {
              foo: Foo__object!
              bar: Bar__object! @inaccessible
            }

            # reachable from Query.foo
            type Foo__object { field: ID! }

            # unreachable from Query.bar
            type Bar__object { baz: Baz__object! }

            # unreachable from Bar__object.baz
            type Baz__object { field: ID! }
            "#,
            &[],
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Query",
            "Query.foo",
            "Foo__object",
            "Foo__object.field",
            // Only the types are masked; their fields are left alone.
            "Bar__object.baz",
            "Baz__object.field",
        ]);
        tester.inaccessible(&["Query.bar", "Bar__object", "Baz__object"]);
    }

    #[test]
    fn does_not_reach_through_an_inaccessible_type() {
        let schema = unreachable_type_masking(
            r#"
            type Query {
              # The field is accessible, so the only thing stopping the walk is
              # @inaccessible on the type it returns.
              foo: Foo__object!
            }

            type Foo__object @inaccessible { baz: Baz__object! }

            # reachable only through the masked Foo__object
            type Baz__object { field: ID! }
            "#,
            &[],
        );

        let tester = Tester::new(&schema);
        tester.accessible(&["Query", "Query.foo"]);
        tester.inaccessible(&["Foo__object", "Baz__object"]);
    }

    #[test]
    fn masks_unreachable_interfaces() {
        let schema = unreachable_type_masking(
            r#"
            type Query {
              field: String
              foo: Foo__interface! @inaccessible
            }

            # unreachable from Query.foo
            interface Foo__interface { field: ID! }

            interface Bar__interface implements Foo__interface {
              field: ID!
              otherField: String
            }

            interface Baz__interface implements Foo__interface & Bar__interface {
              field: ID!
              otherField: String
            }
            "#,
            &[],
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Query",
            "Query.field",
            "Foo__interface.field",
            "Bar__interface.field",
            "Bar__interface.otherField",
            "Baz__interface.field",
            "Baz__interface.otherField",
        ]);
        tester.inaccessible(&[
            "Query.foo",
            "Foo__interface",
            "Bar__interface",
            "Baz__interface",
        ]);
    }

    #[test]
    fn masks_unreachable_unions() {
        let schema = unreachable_type_masking(
            r#"
            type Query {
              field: String!
              foo: Foo__union! @inaccessible
            }

            # unreachable from Query.foo
            union Foo__union = Bar__object | Baz__object

            type Bar__object { field: ID! }
            type Baz__object { field: ID! }
            "#,
            &[],
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Query",
            "Query.field",
            "Bar__object.field",
            "Baz__object.field",
        ]);
        tester.inaccessible(&["Query.foo", "Foo__union", "Bar__object", "Baz__object"]);
    }

    /// A union stays reachable while any of its members is.
    #[test]
    fn leaves_union_reachable_from_an_accessible_member() {
        let schema = unreachable_type_masking(
            r#"
            type Query {
              foo: Foo__union! @inaccessible
              bar: Bar__object
            }

            # Unreachable from Query.foo, but reachable from Query.bar
            union Foo__union = Bar__object | Baz__object

            type Bar__object { field: ID! }
            type Baz__object { field: ID! }
            "#,
            &[],
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Query",
            "Query.bar",
            "Foo__union",
            "Bar__object",
            "Bar__object.field",
            "Baz__object",
            "Baz__object.field",
        ]);
        tester.inaccessible(&["Query.foo"]);
    }

    /// Directives on operation locations keep their argument input types reachable.
    #[test]
    fn treats_operation_directive_argument_input_types_as_reachable() {
        let schema = unreachable_type_masking(
            r#"
            type Query { field: String! }

            directive @directive(
              foo: Foo__scalar!
              bar: Foo__enum!
              baz: Foo__input!
            ) on FRAGMENT_DEFINITION

            scalar Foo__scalar
            enum Foo__enum { ONE TWO }
            input Foo__input { id: ID! }
            "#,
            &[],
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Query",
            "Query.field",
            "@directive(foo:)",
            "@directive(bar:)",
            "@directive(baz:)",
            "Foo__scalar",
            "Foo__enum",
            "Foo__enum.ONE",
            "Foo__enum.TWO",
            "Foo__input",
            "Foo__input.id",
        ]);
    }

    #[test]
    fn does_not_reach_through_an_inaccessible_operation_directive_argument() {
        let schema = unreachable_type_masking(
            r#"
            type Query { field: String! }

            directive @directive(
              foo: Foo__input!
              bar: Bar__input! @inaccessible
            ) on FRAGMENT_DEFINITION

            input Foo__input { id: ID! }

            # reachable only through the masked @directive(bar:)
            input Bar__input { id: ID! }
            "#,
            &[],
        );

        let tester = Tester::new(&schema);
        tester.accessible(&["Query", "Query.field", "Foo__input", "Foo__input.id"]);
        tester.inaccessible(&["@directive(bar:)", "Bar__input"]);
    }

    /// Directives on type-system locations do not.
    #[test]
    fn treats_non_operation_directive_argument_input_types_as_unreachable() {
        let schema = unreachable_type_masking(
            r#"
            type Query { field: String! }

            directive @directive(
              foo: Foo__scalar!
              bar: Foo__enum!
              baz: Foo__input!
            ) on SCHEMA

            scalar Foo__scalar
            enum Foo__enum { ONE TWO }
            input Foo__input { id: ID! }
            "#,
            &[],
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Query",
            "Query.field",
            "@directive(foo:)",
            "@directive(bar:)",
            "@directive(baz:)",
            "Foo__enum.ONE",
            "Foo__enum.TWO",
            "Foo__input.id",
        ]);
        tester.inaccessible(&["Foo__scalar", "Foo__enum", "Foo__input"]);
    }

    #[test]
    fn follows_nested_field_argument_input_types() {
        let schema = unreachable_type_masking(
            r#"
            type Query { foo: Foo__object! }

            # reachable from Query.foo
            type Foo__object { bar: Bar__object! }

            # reachable from Foo__object.bar
            type Bar__object {
              field(
                baz__arg: Baz__input!
                qux__arg: Qux__input! @inaccessible
              ): Foo__object!
            }

            # reachable from Bar__object.field(baz__arg:)
            input Baz__input { field: String! }

            # only reachable through the inaccessible argument
            input Qux__input { field: String! }
            "#,
            &[],
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Query",
            "Query.foo",
            "Foo__object",
            "Foo__object.bar",
            "Bar__object",
            "Bar__object.field",
            "Bar__object.field(baz__arg:)",
            "Baz__input",
            "Baz__input.field",
            "Qux__input.field",
        ]);
        tester.inaccessible(&["Bar__object.field(qux__arg:)", "Qux__input"]);
    }

    /// A type carrying an included tag is a reachability root in its own right.
    #[test]
    fn treats_types_with_an_included_tag_as_reachable() {
        let schema = unreachable_type_masking(
            r#"
            type Query { field: String! }

            # tagged to be included
            type Foo__object @tag(name: "include-me") {
              baz(input: Foo__input): Baz__object!
            }

            # unreachable
            type Bar__object { field: String! }

            # used by Foo__object.baz
            type Baz__object { field: String! }

            # inaccessible already, so the include tag does not resurrect it
            type Qux__object
              @inaccessible
              @tag(name: "include-me")
              @tag(name: "exclude-me") {
              field: String!
            }

            # reachable from Foo__object.baz(input:)
            input Foo__input { field: String }

            # unreachable
            input Bar__input { field: String }

            scalar Foo__scalar @tag(name: "include-me")
            scalar Bar__scalar

            enum Foo__enum @tag(name: "include-me") { ONE TWO }
            enum Bar__enum { ONE TWO }
            "#,
            &["include-me"],
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Query",
            "Query.field",
            "Foo__object",
            "Foo__object.baz",
            "Bar__object.field",
            "Baz__object",
            "Baz__object.field",
            "Qux__object.field",
            "Foo__scalar",
            "Foo__enum",
            "Foo__input",
            "Foo__input.field",
            "Bar__input.field",
        ]);
        tester.inaccessible(&[
            "Bar__object",
            "Qux__object",
            "Bar__scalar",
            "Bar__enum",
            "Bar__input",
        ]);
    }

    /// Core spec types are never masked, but the traversal still walks through them, like
    /// the JS: a type reachable only through a core type is reachable.
    #[test]
    fn reaches_through_core_spec_types() {
        let preamble = Preamble {
            use_bar_spec: true,
            ..Preamble::default()
        };
        let filters = filters_hiding_unreachable(&[]);
        let schema = run_step(
            supergraph_with(
                &preamble,
                r#"
                type Query { core: bar__Wrapper }

                type bar__Wrapper { user: OnlyThroughCore }

                type OnlyThroughCore { field: String }
                "#,
            ),
            |schema, metadata| step12_unreachable_type_masking(schema, metadata, &filters),
        );

        Tester::new(&schema).accessible(&["bar__Wrapper", "OnlyThroughCore"]);
    }

    /// Reaching an interface reaches the interfaces it implements, even with no implementing
    /// object to reach them through. The JS traversal does not follow this edge.
    #[test]
    fn reaches_super_interfaces_through_an_interface() {
        let schema = unreachable_type_masking(
            r#"
            type Query { sub: SubInterface }

            interface SuperInterface { field: String }

            interface SubInterface implements SuperInterface { field: String }
            "#,
            &[],
        );

        let tester = Tester::new(&schema);
        tester.accessible(&["Query", "SubInterface", "SuperInterface"]);
    }
}
