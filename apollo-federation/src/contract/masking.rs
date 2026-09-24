use apollo_compiler::collections::IndexSet;

use super::ContractFilters;
use super::helpers::FilterDirectiveMetadata;
use super::helpers::applied_tags;
use super::helpers::composite_types;
use super::helpers::filterable_types;
use super::helpers::inaccessible_elements;
use super::helpers::is_filterable_type;
use super::helpers::is_inaccessible;
use super::helpers::mark_inaccessible;
use crate::error::FederationError;
use crate::schema::FederationSchema;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;

// Steps 6, 7, 10 and 11 mask a container once every child is `@inaccessible`. A container
// with no `@inaccessible` child cannot qualify, so each starts from the `@inaccessible`
// referencers and only checks the containers they point back to.

/// Step 6: Empty enum masking.
///
/// Marks enum types @inaccessible if ALL their values are @inaccessible.
pub(crate) fn step6_empty_enum_masking(
    schema: &mut FederationSchema,
    metadata: &FilterDirectiveMetadata,
) -> Result<(), FederationError> {
    let candidates: IndexSet<_> = inaccessible_elements(schema, metadata)
        .enum_values
        .iter()
        .map(|value| value.parent())
        .collect();

    for enum_type in candidates {
        if !is_filterable_type(schema.schema(), metadata, &enum_type.type_name) {
            continue;
        }
        let all_inaccessible = enum_type
            .get(schema.schema())?
            .values
            .values()
            .all(|value| value.directives.has(&metadata.inaccessible));
        if all_inaccessible {
            mark_inaccessible(schema, enum_type, metadata)?;
        }
    }
    Ok(())
}

/// Step 7: Empty input object masking.
///
/// Marks input object types @inaccessible if ALL their fields are @inaccessible.
pub(crate) fn step7_empty_input_object_masking(
    schema: &mut FederationSchema,
    metadata: &FilterDirectiveMetadata,
) -> Result<(), FederationError> {
    let candidates: IndexSet<_> = inaccessible_elements(schema, metadata)
        .input_object_fields
        .iter()
        .map(|field| field.parent())
        .collect();

    for input_type in candidates {
        if !is_filterable_type(schema.schema(), metadata, &input_type.type_name) {
            continue;
        }
        let all_inaccessible = input_type
            .get(schema.schema())?
            .fields
            .values()
            .all(|field| field.directives.has(&metadata.inaccessible));
        if all_inaccessible {
            mark_inaccessible(schema, input_type, metadata)?;
        }
    }
    Ok(())
}

/// Step 8: Empty object/interface field masking.
///
/// Applies include filters to object/interface fields. Step 5 skips fields because they can
/// have children, so this is where a field without an included tag gets masked -- unless one
/// of its arguments is still accessible, since masking the field would take an included
/// argument down with it.
///
/// A field is marked @inaccessible when all of these hold:
/// 1. At least one include filter is active.
/// 2. The field is not tagged with an include filter tag.
/// 3. None of its arguments is accessible. A field with no arguments trivially satisfies this.
///
/// Unlike the other masking steps this one also applies to fields with no children at all,
/// so it has to visit every field.
pub(crate) fn step8_empty_field_masking(
    schema: &mut FederationSchema,
    metadata: &FilterDirectiveMetadata,
    filters: &ContractFilters,
) -> Result<(), FederationError> {
    if filters.include.is_empty() {
        return Ok(());
    }

    let mut fields = Vec::new();
    for type_ in composite_types(schema, metadata) {
        fields.extend(type_.fields(schema.schema())?);
    }

    for field in fields {
        let definition = field.get(schema.schema())?;
        // An already-masked field is left alone.
        if definition.directives.has(&metadata.inaccessible) {
            continue;
        }
        let all_args_inaccessible = definition
            .arguments
            .iter()
            .all(|arg| arg.directives.has(&metadata.inaccessible));
        let has_include_tag = applied_tags(&field, schema, metadata)
            .iter()
            .any(|tag| filters.include.contains(tag));
        if all_args_inaccessible && !has_include_tag {
            mark_inaccessible(schema, field, metadata)?;
        }
    }
    Ok(())
}

/// Step 9: Partial interface masking.
///
/// Marks interface fields @inaccessible if ALL implementing object types
/// have that field as effectively inaccessible.
pub(crate) fn step9_partial_interface_masking(
    schema: &mut FederationSchema,
    metadata: &FilterDirectiveMetadata,
) -> Result<(), FederationError> {
    let interfaces: Vec<InterfaceTypeDefinitionPosition> = filterable_types(schema, metadata)
        .filter_map(|type_| match type_ {
            TypeDefinitionPosition::Interface(interface) => Some(interface),
            _ => None,
        })
        .collect();

    for interface in interfaces {
        // Cloned because masking a field below needs the schema mutably.
        let implementers = schema
            .referencers()
            .get_interface_type(&interface.type_name)?
            .object_types
            .clone();
        if implementers.is_empty() {
            continue;
        }

        let fields: Vec<_> = interface.fields(schema.schema())?.collect();
        for field in fields {
            if is_inaccessible(&field, schema, metadata) {
                continue;
            }
            // A field an implementer does not declare is not inaccessible there.
            let all_implementations_inaccessible = implementers.iter().all(|object| {
                is_inaccessible(object, schema, metadata)
                    || is_inaccessible(&object.field(field.field_name.clone()), schema, metadata)
            });
            if all_implementations_inaccessible {
                mark_inaccessible(schema, field, metadata)?;
            }
        }
    }
    Ok(())
}

/// Step 10: Empty object/interface masking.
///
/// Marks object/interface types @inaccessible if ALL their fields are @inaccessible.
pub(crate) fn step10_empty_object_interface_masking(
    schema: &mut FederationSchema,
    metadata: &FilterDirectiveMetadata,
) -> Result<(), FederationError> {
    let candidates: IndexSet<_> = inaccessible_elements(schema, metadata)
        .object_or_interface_fields()
        .map(|field| field.parent())
        .collect();

    for type_ in candidates {
        if !is_filterable_type(schema.schema(), metadata, type_.type_name()) {
            continue;
        }
        let all_inaccessible = type_
            .fields(schema.schema())?
            .all(|field| is_inaccessible(&field, schema, metadata));
        if all_inaccessible {
            mark_inaccessible(schema, type_, metadata)?;
        }
    }
    Ok(())
}

/// Step 11: Empty union masking.
///
/// Marks union types @inaccessible if ALL member types are @inaccessible.
pub(crate) fn step11_empty_union_masking(
    schema: &mut FederationSchema,
    metadata: &FilterDirectiveMetadata,
) -> Result<(), FederationError> {
    let referencers = schema.referencers();
    let candidates: IndexSet<_> = inaccessible_elements(schema, metadata)
        .object_types
        .iter()
        .filter_map(|object| referencers.object_types.get(&object.type_name))
        .flat_map(|object| object.union_types.iter().cloned())
        .collect();

    for union_type in candidates {
        if !is_filterable_type(schema.schema(), metadata, &union_type.type_name) {
            continue;
        }
        let all_inaccessible = union_type
            .get(schema.schema())?
            .members
            .iter()
            .all(|member| {
                is_inaccessible(
                    &ObjectTypeDefinitionPosition::new(member.name.clone()),
                    schema,
                    metadata,
                )
            });
        if all_inaccessible {
            mark_inaccessible(schema, union_type, metadata)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;

    use super::*;
    use crate::contract::testing::Tester;
    use crate::contract::testing::run_step;
    use crate::contract::testing::supergraph;

    /// Run step 6 over `sdl` layered on the composition preamble.
    fn empty_enum_masking(sdl: &str) -> Schema {
        run_step(supergraph(sdl), step6_empty_enum_masking)
    }

    #[test]
    fn marks_enum_inaccessible_when_no_value_is_accessible() {
        let schema = empty_enum_masking(
            r#"
            # Used to check that type references haven't been altered
            type Query {
              field: Enum
            }

            enum Enum {
              VALUE_A @inaccessible
              VALUE_B @inaccessible
              VALUE_C @inaccessible
            }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.accessible(&["Query", "Query.field"]);
        tester.inaccessible(&["Enum", "Enum.VALUE_A", "Enum.VALUE_B", "Enum.VALUE_C"]);
    }

    #[test]
    fn leaves_enum_accessible_when_some_value_is_accessible() {
        let schema = empty_enum_masking(
            r#"
            type Query {
              field: Enum
            }

            enum Enum {
              VALUE_A @inaccessible
              VALUE_B
              VALUE_C @inaccessible
            }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.accessible(&["Query", "Query.field", "Enum", "Enum.VALUE_B"]);
        tester.inaccessible(&["Enum.VALUE_A", "Enum.VALUE_C"]);
    }

    #[test]
    fn preserves_existing_inaccessible_applications_on_enums() {
        let schema = empty_enum_masking(
            r#"
            type Query {
              fieldFoo: EnumFoo
              fieldBar: EnumBar
            }

            enum EnumFoo @inaccessible {
              VALUE_A @inaccessible
              VALUE_B @inaccessible
              VALUE_C @inaccessible
            }

            enum EnumBar @inaccessible {
              VALUE_A @inaccessible
              VALUE_B
              VALUE_C @inaccessible
            }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Query",
            "Query.fieldFoo",
            "Query.fieldBar",
            "EnumBar.VALUE_B",
        ]);
        tester.inaccessible(&[
            "EnumFoo",
            "EnumFoo.VALUE_A",
            "EnumFoo.VALUE_B",
            "EnumFoo.VALUE_C",
            // Already @inaccessible before the step ran, and stays so despite
            // VALUE_B being accessible.
            "EnumBar",
            "EnumBar.VALUE_A",
            "EnumBar.VALUE_C",
        ]);
    }
}

#[cfg(test)]
mod empty_input_object_masking_tests {
    use apollo_compiler::Schema;

    use super::*;
    use crate::contract::testing::Tester;
    use crate::contract::testing::run_step;
    use crate::contract::testing::supergraph;

    /// Run step 7 over `sdl` layered on the composition preamble.
    fn empty_input_object_masking(sdl: &str) -> Schema {
        run_step(supergraph(sdl), step7_empty_input_object_masking)
    }

    #[test]
    fn marks_input_object_inaccessible_when_no_field_is_accessible() {
        let schema = empty_input_object_masking(
            r#"
            # Used to check that type references haven't been altered
            type Query {
              field(argument: Input): String
            }

            input Input {
              fieldA: String @inaccessible
              fieldB: String @inaccessible
              fieldC: String @inaccessible
            }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.accessible(&["Query", "Query.field", "Query.field(argument:)"]);
        tester.inaccessible(&["Input", "Input.fieldA", "Input.fieldB", "Input.fieldC"]);
    }

    #[test]
    fn leaves_input_object_accessible_when_some_field_is_accessible() {
        let schema = empty_input_object_masking(
            r#"
            type Query {
              field(argument: Input): String
            }

            input Input {
              fieldA: String @inaccessible
              fieldB: String @inaccessible
              fieldC: String
            }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Query",
            "Query.field",
            "Query.field(argument:)",
            "Input",
            "Input.fieldC",
        ]);
        tester.inaccessible(&["Input.fieldA", "Input.fieldB"]);
    }

    #[test]
    fn preserves_existing_inaccessible_applications_on_input_objects() {
        let schema = empty_input_object_masking(
            r#"
            type Query {
              fieldFoo(argument: InputFoo): String
              fieldBar(argument: InputBar): String
            }

            input InputFoo @inaccessible {
              fieldA: String @inaccessible
              fieldB: String @inaccessible
              fieldC: String @inaccessible
            }

            input InputBar @inaccessible {
              fieldA: String @inaccessible
              fieldB: String @inaccessible
              fieldC: String
            }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Query",
            "Query.fieldFoo",
            "Query.fieldFoo(argument:)",
            "Query.fieldBar",
            "Query.fieldBar(argument:)",
            "InputBar.fieldC",
        ]);
        tester.inaccessible(&[
            "InputFoo",
            "InputFoo.fieldA",
            "InputFoo.fieldB",
            "InputFoo.fieldC",
            // Already @inaccessible before the step ran, and stays so despite
            // fieldC being accessible.
            "InputBar",
            "InputBar.fieldA",
            "InputBar.fieldB",
        ]);
    }
}

#[cfg(test)]
mod empty_union_masking_tests {
    use apollo_compiler::Schema;

    use super::*;
    use crate::contract::testing::Preamble;
    use crate::contract::testing::Tester;
    use crate::contract::testing::run_step;
    use crate::contract::testing::supergraph_with;

    /// Run step 11 over `sdl`. The `bar` spec is linked so the core-element test has a
    /// namespace to hide behind.
    fn empty_union_masking(sdl: &str) -> Schema {
        let preamble = Preamble {
            use_bar_spec: true,
            ..Preamble::default()
        };
        run_step(supergraph_with(&preamble, sdl), step11_empty_union_masking)
    }

    #[test]
    fn marks_union_inaccessible_when_no_member_is_accessible() {
        let schema = empty_union_masking(
            r#"
            # Used to check that type references haven't been altered
            type Query {
              field: Union
            }

            union Union = ObjectA | ObjectB | ObjectC

            type ObjectA @inaccessible { field: String }
            type ObjectB @inaccessible { field: String }
            type ObjectC @inaccessible { field: String }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Query",
            "Query.field",
            "ObjectA.field",
            "ObjectB.field",
            "ObjectC.field",
        ]);
        tester.inaccessible(&["Union", "ObjectA", "ObjectB", "ObjectC"]);
    }

    #[test]
    fn leaves_union_accessible_when_some_member_is_accessible() {
        let schema = empty_union_masking(
            r#"
            type Query {
              field: Union
            }

            union Union = ObjectA | ObjectB | ObjectC

            type ObjectA @inaccessible { field: String }
            type ObjectB @inaccessible { field: String }
            type ObjectC { field: String }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Query",
            "Query.field",
            "Union",
            "ObjectC",
            "ObjectA.field",
            "ObjectB.field",
            "ObjectC.field",
        ]);
        tester.inaccessible(&["ObjectA", "ObjectB"]);
    }

    /// Core schema elements are exempt from filtering even when every member is masked.
    #[test]
    fn leaves_core_union_accessible_when_no_member_is_accessible() {
        let schema = empty_union_masking(
            r#"
            type Query {
              field: bar__Union
            }

            union bar__Union = ObjectA | ObjectB | ObjectC

            type ObjectA @inaccessible { field: String }
            type ObjectB @inaccessible { field: String }
            type ObjectC @inaccessible { field: String }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Query",
            "Query.field",
            "bar__Union",
            "ObjectA.field",
            "ObjectB.field",
            "ObjectC.field",
        ]);
        tester.inaccessible(&["ObjectA", "ObjectB", "ObjectC"]);
    }

    #[test]
    fn preserves_existing_inaccessible_applications_on_unions() {
        let schema = empty_union_masking(
            r#"
            type Query {
              fieldFoo: UnionFoo
              fieldBar: UnionBar
            }

            union UnionFoo @inaccessible = ObjectA | ObjectB | ObjectC
            union UnionBar @inaccessible = ObjectA | ObjectB | ObjectD

            type ObjectA @inaccessible { field: String }
            type ObjectB @inaccessible { field: String }
            type ObjectC @inaccessible { field: String }
            type ObjectD { field: String }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Query",
            "Query.fieldFoo",
            "Query.fieldBar",
            "ObjectD",
            "ObjectA.field",
            "ObjectB.field",
            "ObjectC.field",
            "ObjectD.field",
        ]);
        tester.inaccessible(&[
            "UnionFoo",
            // Already @inaccessible before the step ran, and stays so despite
            // ObjectD being accessible.
            "UnionBar", "ObjectA", "ObjectB", "ObjectC",
        ]);
    }
}

#[cfg(test)]
mod partial_interface_masking_tests {
    use apollo_compiler::Schema;

    use super::*;
    use crate::contract::testing::Preamble;
    use crate::contract::testing::Tester;
    use crate::contract::testing::run_step;
    use crate::contract::testing::supergraph_with;

    /// Run step 9 over `sdl`. The `bar` spec is linked so the core-element test has a
    /// namespace to hide behind.
    fn partial_interface_masking(sdl: &str) -> Schema {
        let preamble = Preamble {
            use_bar_spec: true,
            ..Preamble::default()
        };
        run_step(
            supergraph_with(&preamble, sdl),
            step9_partial_interface_masking,
        )
    }

    #[test]
    fn masks_interface_field_when_all_implementing_fields_are_inaccessible() {
        let schema = partial_interface_masking(
            r#"
            type Query { field: String }

            interface Interface {
              field: ID!
              otherField: String!
            }

            type ObjectA implements Interface {
              field: ID! @inaccessible
              otherField: String!
            }

            type ObjectB implements Interface {
              field: ID! @inaccessible
              otherField: String!
            }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Interface",
            "Interface.otherField",
            "ObjectA",
            "ObjectA.otherField",
            "ObjectB",
            "ObjectB.otherField",
        ]);
        tester.inaccessible(&["Interface.field", "ObjectA.field", "ObjectB.field"]);
    }

    /// A field whose parent type is `@inaccessible` is "effectively" inaccessible, even
    /// though the field itself carries no directive.
    #[test]
    fn masks_interface_field_when_all_implementing_fields_are_effectively_inaccessible() {
        let schema = partial_interface_masking(
            r#"
            type Query { field: String }

            interface Interface {
              field: ID!
              otherField: String!
            }

            type ObjectA implements Interface {
              field: ID! @inaccessible
              otherField: String!
            }

            type ObjectB implements Interface @inaccessible {
              field: ID!
              otherField: String!
            }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Interface",
            "Interface.otherField",
            "ObjectA",
            "ObjectA.otherField",
            "ObjectB.field",
            "ObjectB.otherField",
        ]);
        tester.inaccessible(&["Interface.field", "ObjectA.field", "ObjectB"]);
    }

    #[test]
    fn masks_every_interface_field_when_all_implementing_types_are_inaccessible() {
        let schema = partial_interface_masking(
            r#"
            type Query { field: String }

            interface Interface {
              field: ID!
              otherField: String!
            }

            type ObjectA implements Interface @inaccessible {
              field: ID!
              otherField: String!
            }

            type ObjectB implements Interface @inaccessible {
              field: ID!
              otherField: String!
            }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Interface",
            "ObjectA.field",
            "ObjectA.otherField",
            "ObjectB.field",
            "ObjectB.otherField",
        ]);
        tester.inaccessible(&[
            "Interface.field",
            "Interface.otherField",
            "ObjectA",
            "ObjectB",
        ]);
    }

    #[test]
    fn masks_nested_interface_field_when_all_implementing_fields_are_inaccessible() {
        let schema = partial_interface_masking(
            r#"
            type Query { field: String }

            interface NestedInterface {
              field: ID!
            }

            interface Interface implements NestedInterface {
              field: ID!
              otherField: String!
            }

            type ObjectA implements NestedInterface {
              field: ID! @inaccessible
            }

            type ObjectB implements Interface & NestedInterface {
              field: ID! @inaccessible
              otherField: String!
            }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "NestedInterface",
            "Interface",
            "Interface.otherField",
            "ObjectA",
            "ObjectB",
            "ObjectB.otherField",
        ]);
        tester.inaccessible(&[
            "NestedInterface.field",
            "Interface.field",
            "ObjectA.field",
            "ObjectB.field",
        ]);
    }

    #[test]
    fn leaves_interface_field_accessible_when_some_implementing_field_is_accessible() {
        let schema = partial_interface_masking(
            r#"
            type Query { field: String }

            interface Interface {
              field: ID!
              otherField: String!
            }

            type ObjectA implements Interface {
              field: ID! @inaccessible
              otherField: String!
            }

            type ObjectB implements Interface {
              field: ID!
              otherField: String! @inaccessible
            }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Interface",
            "Interface.field",
            "Interface.otherField",
            "ObjectA",
            "ObjectA.otherField",
            "ObjectB",
            "ObjectB.field",
        ]);
        tester.inaccessible(&["ObjectA.field", "ObjectB.otherField"]);
    }

    #[test]
    fn leaves_interface_field_accessible_when_no_implementing_field_exists() {
        let schema = partial_interface_masking(
            r#"
            type Query { field: String }

            interface Interface {
              field: ID!
            }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.accessible(&["Interface", "Interface.field"]);
    }

    /// Core schema elements are exempt from filtering.
    #[test]
    fn leaves_core_interface_field_accessible_when_all_implementing_fields_are_inaccessible() {
        let schema = partial_interface_masking(
            r#"
            type Query { field: String }

            interface bar__Interface {
              field: ID!
              otherField: String!
            }

            type ObjectA implements bar__Interface {
              field: ID! @inaccessible
              otherField: String!
            }

            type ObjectB implements bar__Interface {
              field: ID! @inaccessible
              otherField: String!
            }
            "#,
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "bar__Interface",
            "bar__Interface.field",
            "bar__Interface.otherField",
            "ObjectA",
            "ObjectA.otherField",
            "ObjectB",
            "ObjectB.otherField",
        ]);
        tester.inaccessible(&["ObjectA.field", "ObjectB.field"]);
    }
}

#[cfg(test)]
mod empty_field_masking_tests {
    use apollo_compiler::Schema;

    use super::*;
    use crate::contract::testing::Preamble;
    use crate::contract::testing::Tester;
    use crate::contract::testing::filters;
    use crate::contract::testing::run_step;
    use crate::contract::testing::supergraph_with;

    /// Run step 8 over `sdl` with the given include filters.
    fn empty_field_masking(sdl: &str, include: &[&str]) -> Schema {
        empty_field_masking_with(&Preamble::default(), sdl, include)
    }

    fn empty_field_masking_with(preamble: &Preamble, sdl: &str, include: &[&str]) -> Schema {
        let filters = filters(include, &[]);
        run_step(supergraph_with(preamble, sdl), |schema, metadata| {
            step8_empty_field_masking(schema, metadata, &filters)
        })
    }

    /// The object and interface halves of every fixture are identical, so the
    /// assertions always come in pairs.
    const REFERENCES: &[&str] = &["Query", "Query.fieldObject", "Query.fieldInterface"];

    #[test]
    fn masks_field_when_no_argument_is_accessible_and_field_is_not_included() {
        let schema = empty_field_masking(
            r#"
            # Used to check that type references haven't been altered
            type Query {
              fieldObject: Object @tag(name: "public")
              fieldInterface: Interface @tag(name: "public")
            }

            type Object implements Interface {
              field(
                argumentA: String @inaccessible
                argumentB: String @inaccessible
                argumentC: String @inaccessible
              ): String
            }

            interface Interface {
              field(
                argumentA: String @inaccessible
                argumentB: String @inaccessible
                argumentC: String @inaccessible
              ): String
            }
            "#,
            &["public"],
        );

        let tester = Tester::new(&schema);
        tester.accessible(REFERENCES);
        tester.accessible(&["Object", "Interface"]);
        tester.inaccessible(&[
            "Object.field",
            "Object.field(argumentA:)",
            "Object.field(argumentB:)",
            "Object.field(argumentC:)",
            "Interface.field",
            "Interface.field(argumentA:)",
            "Interface.field(argumentB:)",
            "Interface.field(argumentC:)",
        ]);
    }

    #[test]
    fn masks_childless_field_when_field_is_not_included() {
        let schema = empty_field_masking(
            r#"
            type Query {
              fieldObject: Object @tag(name: "public")
              fieldInterface: Interface @tag(name: "public")
            }

            type Object implements Interface { field: String }
            interface Interface { field: String }
            "#,
            &["public"],
        );

        let tester = Tester::new(&schema);
        tester.accessible(REFERENCES);
        tester.accessible(&["Object", "Interface"]);
        tester.inaccessible(&["Object.field", "Interface.field"]);
    }

    #[test]
    fn leaves_field_accessible_when_some_argument_is_accessible() {
        let schema = empty_field_masking(
            r#"
            type Query {
              fieldObject: Object @tag(name: "public")
              fieldInterface: Interface @tag(name: "public")
            }

            type Object implements Interface {
              field(
                argumentA: String
                argumentB: String @inaccessible
                argumentC: String @inaccessible
              ): String
            }

            interface Interface {
              field(
                argumentA: String
                argumentB: String @inaccessible
                argumentC: String @inaccessible
              ): String
            }
            "#,
            &["public"],
        );

        let tester = Tester::new(&schema);
        tester.accessible(REFERENCES);
        tester.accessible(&[
            "Object",
            "Object.field",
            "Object.field(argumentA:)",
            "Interface",
            "Interface.field",
            "Interface.field(argumentA:)",
        ]);
        tester.inaccessible(&[
            "Object.field(argumentB:)",
            "Object.field(argumentC:)",
            "Interface.field(argumentB:)",
            "Interface.field(argumentC:)",
        ]);
    }

    /// An included field is kept even when every one of its arguments is masked.
    #[test]
    fn leaves_included_field_accessible_when_no_argument_is_accessible() {
        let schema = empty_field_masking(
            r#"
            type Query {
              fieldObject: Object @tag(name: "public")
              fieldInterface: Interface @tag(name: "public")
            }

            type Object implements Interface {
              field(
                argumentA: String @inaccessible
                argumentB: String @inaccessible
                argumentC: String @inaccessible
              ): String @tag(name: "public")
            }

            interface Interface {
              field(
                argumentA: String @inaccessible
                argumentB: String @inaccessible
                argumentC: String @inaccessible
              ): String @tag(name: "public")
            }
            "#,
            &["public"],
        );

        let tester = Tester::new(&schema);
        tester.accessible(REFERENCES);
        tester.accessible(&["Object", "Object.field", "Interface", "Interface.field"]);
        tester.inaccessible(&[
            "Object.field(argumentA:)",
            "Object.field(argumentB:)",
            "Object.field(argumentC:)",
            "Interface.field(argumentA:)",
            "Interface.field(argumentB:)",
            "Interface.field(argumentC:)",
        ]);
    }

    #[test]
    fn leaves_included_childless_field_accessible() {
        let schema = empty_field_masking(
            r#"
            type Query {
              fieldObject: Object @tag(name: "public")
              fieldInterface: Interface @tag(name: "public")
            }

            type Object implements Interface { field: String @tag(name: "public") }
            interface Interface { field: String @tag(name: "public") }
            "#,
            &["public"],
        );

        let tester = Tester::new(&schema);
        tester.accessible(REFERENCES);
        tester.accessible(&["Object", "Object.field", "Interface", "Interface.field"]);
    }

    /// With no include filters nothing is "not included", so no field is masked.
    #[test]
    fn leaves_field_accessible_when_there_are_no_include_filters() {
        let schema = empty_field_masking(
            r#"
            type Query {
              fieldObject: Object
              fieldInterface: Interface
            }

            type Object implements Interface {
              field(
                argumentA: String @inaccessible
                argumentB: String @inaccessible
                argumentC: String @inaccessible
              ): String
            }

            interface Interface {
              field(
                argumentA: String @inaccessible
                argumentB: String @inaccessible
                argumentC: String @inaccessible
              ): String
            }
            "#,
            &[],
        );

        let tester = Tester::new(&schema);
        tester.accessible(REFERENCES);
        tester.accessible(&["Object", "Object.field", "Interface", "Interface.field"]);
        tester.inaccessible(&[
            "Object.field(argumentA:)",
            "Object.field(argumentB:)",
            "Object.field(argumentC:)",
            "Interface.field(argumentA:)",
            "Interface.field(argumentB:)",
            "Interface.field(argumentC:)",
        ]);
    }

    #[test]
    fn leaves_childless_field_accessible_when_there_are_no_include_filters() {
        let schema = empty_field_masking(
            r#"
            type Query {
              fieldObject: Object
              fieldInterface: Interface
            }

            type Object implements Interface { field: String }
            interface Interface { field: String }
            "#,
            &[],
        );

        let tester = Tester::new(&schema);
        tester.accessible(REFERENCES);
        tester.accessible(&["Object", "Object.field", "Interface", "Interface.field"]);
    }

    /// Core schema elements are exempt from filtering.
    #[test]
    fn leaves_core_parent_type_field_accessible_when_field_is_not_included() {
        let preamble = Preamble {
            use_bar_spec: true,
            ..Preamble::default()
        };
        let schema = empty_field_masking_with(
            &preamble,
            r#"
            type Query {
              fieldObject: bar__Object @tag(name: "public")
              fieldInterface: bar__Interface @tag(name: "public")
            }

            type bar__Object implements bar__Interface { field: String }
            interface bar__Interface { field: String }
            "#,
            &["public"],
        );

        let tester = Tester::new(&schema);
        tester.accessible(REFERENCES);
        tester.accessible(&[
            "bar__Object",
            "bar__Object.field",
            "bar__Interface",
            "bar__Interface.field",
        ]);
    }

    #[test]
    fn preserves_existing_inaccessible_applications_on_fields() {
        let schema = empty_field_masking(
            r#"
            type Query {
              fieldObjectFoo: ObjectFoo @tag(name: "public")
              fieldInterfaceFoo: InterfaceFoo @tag(name: "public")
              fieldObjectBar: ObjectBar @tag(name: "public")
              fieldInterfaceBar: InterfaceBar @tag(name: "public")
            }

            type ObjectFoo implements InterfaceFoo {
              field(
                argumentA: String @inaccessible
                argumentB: String @inaccessible
                argumentC: String @inaccessible
              ): String @inaccessible
            }

            interface InterfaceFoo {
              field(
                argumentA: String @inaccessible
                argumentB: String @inaccessible
                argumentC: String @inaccessible
              ): String @inaccessible
            }

            type ObjectBar implements InterfaceBar {
              field(
                argumentA: String @inaccessible
                argumentB: String
                argumentC: String @inaccessible
              ): String @inaccessible
            }

            interface InterfaceBar {
              field(
                argumentA: String @inaccessible
                argumentB: String
                argumentC: String @inaccessible
              ): String @inaccessible
            }
            "#,
            &["public"],
        );

        let tester = Tester::new(&schema);
        tester.accessible(&[
            "Query",
            "Query.fieldObjectFoo",
            "Query.fieldInterfaceFoo",
            "Query.fieldObjectBar",
            "Query.fieldInterfaceBar",
            "ObjectFoo",
            "InterfaceFoo",
            "ObjectBar",
            "InterfaceBar",
            // Already @inaccessible on the field before the step ran, so the
            // accessible argument is left alone.
            "ObjectBar.field(argumentB:)",
            "InterfaceBar.field(argumentB:)",
        ]);
        tester.inaccessible(&[
            "ObjectFoo.field",
            "ObjectFoo.field(argumentA:)",
            "ObjectFoo.field(argumentB:)",
            "ObjectFoo.field(argumentC:)",
            "InterfaceFoo.field",
            "InterfaceFoo.field(argumentA:)",
            "InterfaceFoo.field(argumentB:)",
            "InterfaceFoo.field(argumentC:)",
            "ObjectBar.field",
            "ObjectBar.field(argumentA:)",
            "ObjectBar.field(argumentC:)",
            "InterfaceBar.field",
            "InterfaceBar.field(argumentA:)",
            "InterfaceBar.field(argumentC:)",
        ]);
    }
}
