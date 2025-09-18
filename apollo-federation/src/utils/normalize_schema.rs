use std::cmp::Ordering;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ComponentName;
use apollo_compiler::schema::ComponentOrigin;
use apollo_compiler::validation::Valid;
use itertools::Itertools;
use itertools::kmerge_by;

/// For any two [Schema]s that are considered "equal", normalizing them with this function will make
/// it such that:
/// 1. They compare as equal via [PartialEq]/[Eq].
/// 2. They serialize to the same string via [std::fmt::Display].
///
/// Schema "equality" in this context is invariant to the order of:
/// - Schema definitions/extensions, type definitions/extensions, and directive definitions
/// - Field definitions
/// - Argument definitions
/// - Input field definitions
/// - Enum value definitions
/// - Root operation types in schema definitions/extensions
/// - Members in union definitions/extensions
/// - Implemented interfaces in object/interface definitions/extensions
/// - Locations in directive definitions
/// - Directive applications
/// - Arguments of directive applications
/// - Input fields in arguments of directive applications
/// - Input fields in default values of argument/input field definitions
///
/// Note that [PartialEq]/[Eq] ignores whether a component comes from a schema/type definition or a
/// schema/type extension, while [std::fmt::Display] serializes components per-extension.
/// Accordingly, it may be preferable to serialize via [std::fmt::Display] to check for equality if
/// component origin is relevant. We support this by specifically sorting component containers (e.g.
/// directive lists) by content first, and then by component origin (where component origin sort
/// order is determined by the content with that origin).
///
/// Also note that [Schema] uses vectors for (and accordingly [PartialEq]/[Eq] does not ignore the
/// order of):
/// - Argument definitions
/// - Locations in directive definitions
/// - Directive applications
/// - Arguments of directive applications
/// - Input fields in arguments of directive applications
/// - Input fields in default values of argument/input field definitions
pub fn normalize_schema(mut schema: Schema) -> Normalized<Schema> {
    sort_schema_definition(&mut schema.schema_definition);
    schema
        .types
        .values_mut()
        .for_each(sort_extended_type_definition);
    schema.types.sort_unstable_keys();
    schema
        .directive_definitions
        .values_mut()
        .for_each(sort_directive_definition);
    schema.directive_definitions.sort_unstable_keys();
    Normalized(schema)
}

/// The same as [normalize_schema], but for [Valid] [Schema]s. See that function's doc comment for
/// details.
pub fn normalize_valid_schema(schema: Valid<Schema>) -> Normalized<Valid<Schema>> {
    let schema = normalize_schema(schema.into_inner());
    // Schema normalization is just sorting, and does not affect GraphQL spec validity.
    Normalized(Valid::assume_valid(schema.into_inner()))
}

/// A marker wrapper that indicates the contained schema has been normalized via [normalize_schema].
#[derive(Debug, Clone, Eq, PartialEq)]
#[repr(transparent)]
pub struct Normalized<T>(T);

impl<T> Normalized<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> std::ops::Deref for Normalized<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> AsRef<T> for Normalized<T> {
    fn as_ref(&self) -> &T {
        &self.0
    }
}

impl<T: std::fmt::Display> std::fmt::Display for Normalized<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

fn sort_schema_definition(definition: &mut Node<apollo_compiler::schema::SchemaDefinition>) {
    let definition = definition.make_mut();
    let grouped_query = group_components_by_origin_and_sort(
        definition.query.take(),
        |name| name.origin.clone(),
        |_| {},
        compare_component_names,
    );
    let grouped_mutation = group_components_by_origin_and_sort(
        definition.mutation.take(),
        |name| name.origin.clone(),
        |_| {},
        compare_component_names,
    );
    let grouped_subscription = group_components_by_origin_and_sort(
        definition.subscription.take(),
        |name| name.origin.clone(),
        |_| {},
        compare_component_names,
    );
    let grouped_directives = group_components_by_origin_and_sort(
        definition.directives.drain(..),
        |directive| directive.origin.clone(),
        sort_component_directive,
        compare_sorted_component_directives,
    );
    let mut origins: IndexSet<ComponentOrigin> = grouped_query
        .keys()
        .chain(grouped_mutation.keys())
        .chain(grouped_subscription.keys())
        .chain(grouped_directives.keys())
        .cloned()
        .collect();
    sort_origins(&mut origins, |left, right| {
        match compare_origins_by_sorted_components(
            left,
            right,
            &grouped_query,
            compare_component_names,
        ) {
            Ordering::Equal => (),
            non_equal => return non_equal,
        }
        match compare_origins_by_sorted_components(
            left,
            right,
            &grouped_mutation,
            compare_component_names,
        ) {
            Ordering::Equal => (),
            non_equal => return non_equal,
        }
        match compare_origins_by_sorted_components(
            left,
            right,
            &grouped_subscription,
            compare_component_names,
        ) {
            Ordering::Equal => (),
            non_equal => return non_equal,
        }
        compare_origins_by_sorted_components(
            left,
            right,
            &grouped_directives,
            compare_sorted_component_directives,
        )
    });
    definition.query = kmerge_sorted_components_and_origins(
        grouped_query,
        &origins,
        |name| &name.origin,
        compare_component_names,
    )
    .next();
    definition.mutation = kmerge_sorted_components_and_origins(
        grouped_mutation,
        &origins,
        |name| &name.origin,
        compare_component_names,
    )
    .next();
    definition.subscription = kmerge_sorted_components_and_origins(
        grouped_subscription,
        &origins,
        |name| &name.origin,
        compare_component_names,
    )
    .next();
    definition
        .directives
        .extend(kmerge_sorted_components_and_origins(
            grouped_directives,
            &origins,
            |directive| &directive.origin,
            compare_sorted_component_directives,
        ));
}

fn sort_extended_type_definition(definition: &mut apollo_compiler::schema::ExtendedType) {
    use apollo_compiler::schema::ExtendedType;
    match definition {
        ExtendedType::Scalar(definition) => sort_scalar_type_definition(definition),
        ExtendedType::Object(definition) => sort_object_type_definition(definition),
        ExtendedType::Interface(definition) => sort_interface_type_definition(definition),
        ExtendedType::Union(definition) => sort_union_type_definition(definition),
        ExtendedType::Enum(definition) => sort_enum_type_definition(definition),
        ExtendedType::InputObject(definition) => sort_input_object_type_definition(definition),
    }
}

fn sort_scalar_type_definition(definition: &mut Node<apollo_compiler::schema::ScalarType>) {
    let definition = definition.make_mut();
    let grouped_directives = group_components_by_origin_and_sort(
        definition.directives.drain(..),
        |directive| directive.origin.clone(),
        sort_component_directive,
        compare_sorted_component_directives,
    );
    let mut origins: IndexSet<ComponentOrigin> = grouped_directives.keys().cloned().collect();
    sort_origins(&mut origins, |left, right| {
        compare_origins_by_sorted_components(
            left,
            right,
            &grouped_directives,
            compare_sorted_component_directives,
        )
    });
    definition
        .directives
        .extend(kmerge_sorted_components_and_origins(
            grouped_directives,
            &origins,
            |directive| &directive.origin,
            compare_sorted_component_directives,
        ));
}

fn sort_object_type_definition(definition: &mut Node<apollo_compiler::schema::ObjectType>) {
    let definition = definition.make_mut();
    let grouped_fields = group_components_by_origin_and_sort(
        definition.fields.drain(..),
        |(_, field)| field.origin.clone(),
        sort_component_field_definition,
        compare_sorted_component_field_definitions,
    );
    let grouped_implements_interfaces = group_components_by_origin_and_sort(
        definition.implements_interfaces.drain(..),
        |implements_interface| implements_interface.origin.clone(),
        |_| {},
        compare_component_names,
    );
    let grouped_directives = group_components_by_origin_and_sort(
        definition.directives.drain(..),
        |directive| directive.origin.clone(),
        sort_component_directive,
        compare_sorted_component_directives,
    );
    let mut origins: IndexSet<ComponentOrigin> = grouped_fields
        .keys()
        .chain(grouped_implements_interfaces.keys())
        .chain(grouped_directives.keys())
        .cloned()
        .collect();
    sort_origins(&mut origins, |left, right| {
        match compare_origins_by_sorted_components(
            left,
            right,
            &grouped_fields,
            compare_sorted_component_field_definitions,
        ) {
            Ordering::Equal => (),
            non_equal => return non_equal,
        }
        match compare_origins_by_sorted_components(
            left,
            right,
            &grouped_implements_interfaces,
            compare_component_names,
        ) {
            Ordering::Equal => (),
            non_equal => return non_equal,
        }
        compare_origins_by_sorted_components(
            left,
            right,
            &grouped_directives,
            compare_sorted_component_directives,
        )
    });
    definition
        .fields
        .extend(kmerge_sorted_components_and_origins(
            grouped_fields,
            &origins,
            |(_, field)| &field.origin,
            compare_sorted_component_field_definitions,
        ));
    definition
        .implements_interfaces
        .extend(kmerge_sorted_components_and_origins(
            grouped_implements_interfaces,
            &origins,
            |implements_interface| &implements_interface.origin,
            compare_component_names,
        ));
    definition
        .directives
        .extend(kmerge_sorted_components_and_origins(
            grouped_directives,
            &origins,
            |directive| &directive.origin,
            compare_sorted_component_directives,
        ));
}

fn sort_interface_type_definition(definition: &mut Node<apollo_compiler::schema::InterfaceType>) {
    let definition = definition.make_mut();
    let grouped_fields = group_components_by_origin_and_sort(
        definition.fields.drain(..),
        |(_, field)| field.origin.clone(),
        sort_component_field_definition,
        compare_sorted_component_field_definitions,
    );
    let grouped_implements_interfaces = group_components_by_origin_and_sort(
        definition.implements_interfaces.drain(..),
        |implements_interface| implements_interface.origin.clone(),
        |_| {},
        compare_component_names,
    );
    let grouped_directives = group_components_by_origin_and_sort(
        definition.directives.drain(..),
        |directive| directive.origin.clone(),
        sort_component_directive,
        compare_sorted_component_directives,
    );
    let mut origins: IndexSet<ComponentOrigin> = grouped_fields
        .keys()
        .chain(grouped_implements_interfaces.keys())
        .chain(grouped_directives.keys())
        .cloned()
        .collect();
    sort_origins(&mut origins, |left, right| {
        match compare_origins_by_sorted_components(
            left,
            right,
            &grouped_fields,
            compare_sorted_component_field_definitions,
        ) {
            Ordering::Equal => (),
            non_equal => return non_equal,
        }
        match compare_origins_by_sorted_components(
            left,
            right,
            &grouped_implements_interfaces,
            compare_component_names,
        ) {
            Ordering::Equal => (),
            non_equal => return non_equal,
        }
        compare_origins_by_sorted_components(
            left,
            right,
            &grouped_directives,
            compare_sorted_component_directives,
        )
    });
    definition
        .fields
        .extend(kmerge_sorted_components_and_origins(
            grouped_fields,
            &origins,
            |(_, field)| &field.origin,
            compare_sorted_component_field_definitions,
        ));
    definition
        .implements_interfaces
        .extend(kmerge_sorted_components_and_origins(
            grouped_implements_interfaces,
            &origins,
            |implements_interface| &implements_interface.origin,
            compare_component_names,
        ));
    definition
        .directives
        .extend(kmerge_sorted_components_and_origins(
            grouped_directives,
            &origins,
            |directive| &directive.origin,
            compare_sorted_component_directives,
        ));
}

fn sort_union_type_definition(definition: &mut Node<apollo_compiler::schema::UnionType>) {
    let definition = definition.make_mut();
    let grouped_members = group_components_by_origin_and_sort(
        definition.members.drain(..),
        |member| member.origin.clone(),
        |_| {},
        compare_component_names,
    );
    let grouped_directives = group_components_by_origin_and_sort(
        definition.directives.drain(..),
        |directive| directive.origin.clone(),
        sort_component_directive,
        compare_sorted_component_directives,
    );
    let mut origins: IndexSet<ComponentOrigin> = grouped_members
        .keys()
        .chain(grouped_directives.keys())
        .cloned()
        .collect();
    sort_origins(&mut origins, |left, right| {
        match compare_origins_by_sorted_components(
            left,
            right,
            &grouped_members,
            compare_component_names,
        ) {
            Ordering::Equal => (),
            non_equal => return non_equal,
        }
        compare_origins_by_sorted_components(
            left,
            right,
            &grouped_directives,
            compare_sorted_component_directives,
        )
    });
    definition
        .members
        .extend(kmerge_sorted_components_and_origins(
            grouped_members,
            &origins,
            |member| &member.origin,
            compare_component_names,
        ));
    definition
        .directives
        .extend(kmerge_sorted_components_and_origins(
            grouped_directives,
            &origins,
            |directive| &directive.origin,
            compare_sorted_component_directives,
        ));
}

fn sort_enum_type_definition(definition: &mut Node<apollo_compiler::schema::EnumType>) {
    let definition = definition.make_mut();
    let grouped_values = group_components_by_origin_and_sort(
        definition.values.drain(..),
        |(_, value)| value.origin.clone(),
        sort_component_enum_value_definition,
        compare_sorted_component_enum_value_definitions,
    );
    let grouped_directives = group_components_by_origin_and_sort(
        definition.directives.drain(..),
        |directive| directive.origin.clone(),
        sort_component_directive,
        compare_sorted_component_directives,
    );
    let mut origins: IndexSet<ComponentOrigin> = grouped_values
        .keys()
        .chain(grouped_directives.keys())
        .cloned()
        .collect();
    sort_origins(&mut origins, |left, right| {
        match compare_origins_by_sorted_components(
            left,
            right,
            &grouped_values,
            compare_sorted_component_enum_value_definitions,
        ) {
            Ordering::Equal => (),
            non_equal => return non_equal,
        }
        compare_origins_by_sorted_components(
            left,
            right,
            &grouped_directives,
            compare_sorted_component_directives,
        )
    });
    definition
        .values
        .extend(kmerge_sorted_components_and_origins(
            grouped_values,
            &origins,
            |(_, value)| &value.origin,
            compare_sorted_component_enum_value_definitions,
        ));
    definition
        .directives
        .extend(kmerge_sorted_components_and_origins(
            grouped_directives,
            &origins,
            |directive| &directive.origin,
            compare_sorted_component_directives,
        ));
}

fn sort_input_object_type_definition(
    definition: &mut Node<apollo_compiler::schema::InputObjectType>,
) {
    let definition = definition.make_mut();
    let grouped_fields = group_components_by_origin_and_sort(
        definition.fields.drain(..),
        |(_, field)| field.origin.clone(),
        sort_component_input_value_definition,
        compare_sorted_component_input_value_definitions,
    );
    let grouped_directives = group_components_by_origin_and_sort(
        definition.directives.drain(..),
        |directive| directive.origin.clone(),
        sort_component_directive,
        compare_sorted_component_directives,
    );
    let mut origins: IndexSet<ComponentOrigin> = grouped_fields
        .keys()
        .chain(grouped_directives.keys())
        .cloned()
        .collect();
    sort_origins(&mut origins, |left, right| {
        match compare_origins_by_sorted_components(
            left,
            right,
            &grouped_fields,
            compare_sorted_component_input_value_definitions,
        ) {
            Ordering::Equal => (),
            non_equal => return non_equal,
        }
        compare_origins_by_sorted_components(
            left,
            right,
            &grouped_directives,
            compare_sorted_component_directives,
        )
    });
    definition
        .fields
        .extend(kmerge_sorted_components_and_origins(
            grouped_fields,
            &origins,
            |(_, value)| &value.origin,
            compare_sorted_component_input_value_definitions,
        ));
    definition
        .directives
        .extend(kmerge_sorted_components_and_origins(
            grouped_directives,
            &origins,
            |directive| &directive.origin,
            compare_sorted_component_directives,
        ));
}

fn sort_component_field_definition(
    definition: &mut (Name, Component<apollo_compiler::schema::FieldDefinition>),
) {
    let definition = definition.1.make_mut();
    sort_slice(
        &mut definition.arguments,
        sort_input_value_definition,
        compare_sorted_input_value_definitions,
    );
    sort_slice(
        &mut definition.directives,
        sort_directive,
        compare_sorted_directives,
    );
}

fn compare_sorted_component_field_definitions(
    left: &(Name, Component<apollo_compiler::schema::FieldDefinition>),
    right: &(Name, Component<apollo_compiler::schema::FieldDefinition>),
) -> Ordering {
    let left = &left.1;
    let right = &right.1;
    match left.name.cmp(&right.name) {
        Ordering::Equal => (),
        non_equal => return non_equal,
    }
    match compare_types(&left.ty, &right.ty) {
        Ordering::Equal => (),
        non_equal => return non_equal,
    }
    match compare_slices(
        &left.arguments,
        &right.arguments,
        compare_sorted_input_value_definitions,
    ) {
        Ordering::Equal => (),
        non_equal => return non_equal,
    }
    match compare_slices(
        &left.directives,
        &right.directives,
        compare_sorted_directives,
    ) {
        Ordering::Equal => (),
        non_equal => return non_equal,
    }
    compare_options(&left.description, &right.description, compare_descriptions)
}

fn sort_component_input_value_definition(
    definition: &mut (Name, Component<apollo_compiler::ast::InputValueDefinition>),
) {
    sort_input_value_definition(&mut definition.1);
}

fn compare_sorted_component_input_value_definitions(
    left: &(Name, Component<apollo_compiler::ast::InputValueDefinition>),
    right: &(Name, Component<apollo_compiler::ast::InputValueDefinition>),
) -> Ordering {
    compare_sorted_input_value_definitions(&left.1, &right.1)
}

fn sort_component_enum_value_definition(
    definition: &mut (Name, Component<apollo_compiler::ast::EnumValueDefinition>),
) {
    sort_slice(
        &mut definition.1.make_mut().directives,
        sort_directive,
        compare_sorted_directives,
    );
}

fn compare_sorted_component_enum_value_definitions(
    left: &(Name, Component<apollo_compiler::ast::EnumValueDefinition>),
    right: &(Name, Component<apollo_compiler::ast::EnumValueDefinition>),
) -> Ordering {
    let left = &left.1;
    let right = &right.1;
    match left.value.cmp(&right.value) {
        Ordering::Equal => (),
        non_equal => return non_equal,
    }
    match compare_slices(
        &left.directives,
        &right.directives,
        compare_sorted_directives,
    ) {
        Ordering::Equal => (),
        non_equal => return non_equal,
    }
    compare_options(&left.description, &right.description, compare_descriptions)
}

fn sort_component_directive(directive: &mut Component<apollo_compiler::ast::Directive>) {
    sort_directive(directive);
}

fn compare_sorted_component_directives(
    left: &Component<apollo_compiler::ast::Directive>,
    right: &Component<apollo_compiler::ast::Directive>,
) -> Ordering {
    compare_sorted_directives(left, right)
}

fn compare_component_names(left: &ComponentName, right: &ComponentName) -> Ordering {
    left.name.cmp(&right.name)
}

fn group_components_by_origin_and_sort<T>(
    iter: impl IntoIterator<Item = T>,
    mut origin: impl FnMut(&T) -> ComponentOrigin,
    mut sort: impl FnMut(&mut T),
    mut compare: impl FnMut(&T, &T) -> Ordering,
) -> IndexMap<ComponentOrigin, Vec<T>> {
    iter.into_iter()
        .chunk_by(|component| origin(component))
        .into_iter()
        .map(|(origin, chunk)| {
            let mut chunk: Vec<T> = chunk.collect();
            sort_slice(&mut chunk, &mut sort, &mut compare);
            (origin, chunk)
        })
        .collect()
}

fn sort_origins(
    origins: &mut IndexSet<ComponentOrigin>,
    mut compare: impl FnMut(&ComponentOrigin, &ComponentOrigin) -> Ordering,
) {
    origins.sort_unstable_by(|left, right| {
        match (left, right) {
            (ComponentOrigin::Definition, ComponentOrigin::Extension(_)) => return Ordering::Less,
            (ComponentOrigin::Extension(_), ComponentOrigin::Definition) => {
                return Ordering::Greater;
            }
            _ => (),
        }
        compare(left, right)
    })
}

fn compare_origins_by_sorted_components<T>(
    left: &ComponentOrigin,
    right: &ComponentOrigin,
    components: &IndexMap<ComponentOrigin, Vec<T>>,
    compare: impl FnMut(&T, &T) -> Ordering,
) -> Ordering {
    compare_slices(
        components.get(left).unwrap_or(&vec![]),
        components.get(right).unwrap_or(&vec![]),
        compare,
    )
}

fn kmerge_sorted_components_and_origins<T>(
    components: IndexMap<ComponentOrigin, Vec<T>>,
    origins: &IndexSet<ComponentOrigin>,
    mut origin: impl FnMut(&T) -> &ComponentOrigin,
    mut compare: impl FnMut(&T, &T) -> Ordering,
) -> impl Iterator<Item = T> {
    kmerge_by(components.into_values(), move |left: &T, right: &T| {
        match compare(left, right) {
            Ordering::Equal => (),
            non_equal => return non_equal == Ordering::Less,
        }
        origins
            .get_index_of(origin(left))
            .cmp(&origins.get_index_of(origin(right)))
            == Ordering::Less
    })
}

fn sort_directive_definition(definition: &mut Node<apollo_compiler::ast::DirectiveDefinition>) {
    let definition = definition.make_mut();
    sort_slice(
        &mut definition.arguments,
        sort_input_value_definition,
        compare_sorted_input_value_definitions,
    );
    definition
        .locations
        .sort_unstable_by_key(discriminant_directive_location);
}

fn sort_input_value_definition(definition: &mut Node<apollo_compiler::ast::InputValueDefinition>) {
    let definition = definition.make_mut();
    sort_option(&mut definition.default_value, sort_value);
    sort_slice(
        &mut definition.directives,
        sort_directive,
        compare_sorted_directives,
    );
}

fn compare_sorted_input_value_definitions(
    left: &Node<apollo_compiler::ast::InputValueDefinition>,
    right: &Node<apollo_compiler::ast::InputValueDefinition>,
) -> Ordering {
    match left.name.cmp(&right.name) {
        Ordering::Equal => (),
        non_equal => return non_equal,
    }
    match compare_types(&left.ty, &right.ty) {
        Ordering::Equal => (),
        non_equal => return non_equal,
    }
    match compare_options(
        &left.default_value,
        &right.default_value,
        compare_sorted_values,
    ) {
        Ordering::Equal => (),
        non_equal => return non_equal,
    }
    match compare_slices(
        &left.directives,
        &right.directives,
        compare_sorted_directives,
    ) {
        Ordering::Equal => (),
        non_equal => return non_equal,
    }
    compare_options(&left.description, &right.description, compare_descriptions)
}

fn sort_directive(directive: &mut Node<apollo_compiler::ast::Directive>) {
    sort_slice(
        &mut directive.make_mut().arguments,
        sort_argument,
        compare_sorted_arguments,
    );
}

fn compare_sorted_directives(
    left: &Node<apollo_compiler::ast::Directive>,
    right: &Node<apollo_compiler::ast::Directive>,
) -> Ordering {
    match left.name.cmp(&right.name) {
        Ordering::Equal => (),
        non_equal => return non_equal,
    }
    compare_slices(&left.arguments, &right.arguments, compare_sorted_arguments)
}

fn sort_argument(argument: &mut Node<apollo_compiler::ast::Argument>) {
    sort_value(&mut argument.make_mut().value);
}

fn compare_sorted_arguments(
    left: &Node<apollo_compiler::ast::Argument>,
    right: &Node<apollo_compiler::ast::Argument>,
) -> Ordering {
    match left.name.cmp(&right.name) {
        Ordering::Equal => (),
        non_equal => return non_equal,
    }
    compare_sorted_values(&left.value, &right.value)
}

fn sort_value(value: &mut Node<apollo_compiler::ast::Value>) {
    use apollo_compiler::ast::Value;
    match value.make_mut() {
        Value::Null
        | Value::Enum(_)
        | Value::Variable(_)
        | Value::String(_)
        | Value::Float(_)
        | Value::Int(_)
        | Value::Boolean(_) => {}
        Value::List(values) => {
            // Unlike most lists in schemas, order matters for value lists.
            sort_ordered_slice(values, sort_value);
        }
        Value::Object(values) => {
            sort_slice(values, sort_key_value, compare_sorted_key_values);
        }
    }
}

fn compare_sorted_values(
    left: &Node<apollo_compiler::ast::Value>,
    right: &Node<apollo_compiler::ast::Value>,
) -> Ordering {
    use apollo_compiler::ast::Value;
    match (left.as_ref(), &right.as_ref()) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Enum(left), Value::Enum(right)) => left.cmp(right),
        (Value::Variable(left), Value::Variable(right)) => left.cmp(right),
        (Value::String(left), Value::String(right)) => left.cmp(right),
        (Value::Float(left), Value::Float(right)) => left.as_str().cmp(right.as_str()),
        (Value::Int(left), Value::Int(right)) => left.as_str().cmp(right.as_str()),
        (Value::Boolean(left), Value::Boolean(right)) => left.cmp(right),
        (Value::List(left), Value::List(right)) => {
            compare_slices(left, right, compare_sorted_values)
        }
        (Value::Object(left), Value::Object(right)) => {
            compare_slices(left, right, compare_sorted_key_values)
        }
        (left, right) => discriminant_value(left).cmp(&discriminant_value(right)),
    }
}

fn sort_key_value(key_value: &mut (Name, Node<apollo_compiler::ast::Value>)) {
    sort_value(&mut key_value.1)
}

fn compare_sorted_key_values(
    left: &(Name, Node<apollo_compiler::ast::Value>),
    right: &(Name, Node<apollo_compiler::ast::Value>),
) -> Ordering {
    match left.0.cmp(&right.0) {
        Ordering::Equal => (),
        non_equal => return non_equal,
    }
    compare_sorted_values(&left.1, &right.1)
}

fn compare_types(
    left: &apollo_compiler::ast::Type,
    right: &apollo_compiler::ast::Type,
) -> Ordering {
    use apollo_compiler::ast::Type;
    match (left, right) {
        (Type::Named(left), Type::Named(right)) => left.cmp(right),
        (Type::NonNullNamed(left), Type::NonNullNamed(right)) => left.cmp(right),
        (Type::List(left), Type::List(right)) => compare_types(left, right),
        (Type::NonNullList(left), Type::NonNullList(right)) => compare_types(left, right),
        (left, right) => discriminant_type(left).cmp(&discriminant_type(right)),
    }
}

fn compare_descriptions(left: &Node<str>, right: &Node<str>) -> Ordering {
    left.cmp(right)
}

fn sort_slice<T>(
    slice: &mut [T],
    sort: impl FnMut(&mut T),
    compare: impl FnMut(&T, &T) -> Ordering,
) {
    sort_ordered_slice(slice, sort);
    slice.sort_unstable_by(compare);
}

fn sort_ordered_slice<T>(slice: &mut [T], sort: impl FnMut(&mut T)) {
    slice.iter_mut().for_each(sort);
}

/// Based on the [PartialOrd] impl for slices.
fn compare_slices<T>(
    left: &[T],
    right: &[T],
    mut compare: impl FnMut(&T, &T) -> Ordering,
) -> Ordering {
    // Slice to the loop iteration range to enable bounds check elimination in the compiler.
    let len_common = std::cmp::min(left.len(), right.len());
    let left_common = &left[..len_common];
    let right_common = &right[..len_common];
    for i in 0..len_common {
        match compare(&left_common[i], &right_common[i]) {
            Ordering::Equal => continue,
            non_equal => return non_equal,
        }
    }
    left.len().cmp(&right.len())
}

fn sort_option<T>(option: &mut Option<T>, sort: impl FnMut(&mut T)) {
    option.iter_mut().for_each(sort);
}

/// Based on the [PartialOrd] impl for [Option]s.
fn compare_options<T>(
    left: &Option<T>,
    right: &Option<T>,
    mut compare: impl FnMut(&T, &T) -> Ordering,
) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => compare(left, right),
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => Ordering::Equal,
    }
}

fn discriminant_directive_location(location: &apollo_compiler::ast::DirectiveLocation) -> u8 {
    use apollo_compiler::ast::DirectiveLocation;
    match location {
        DirectiveLocation::Query => 0,
        DirectiveLocation::Mutation => 1,
        DirectiveLocation::Subscription => 2,
        DirectiveLocation::Field => 3,
        DirectiveLocation::FragmentDefinition => 4,
        DirectiveLocation::FragmentSpread => 5,
        DirectiveLocation::InlineFragment => 6,
        DirectiveLocation::VariableDefinition => 7,
        DirectiveLocation::Schema => 8,
        DirectiveLocation::Scalar => 9,
        DirectiveLocation::Object => 10,
        DirectiveLocation::FieldDefinition => 11,
        DirectiveLocation::ArgumentDefinition => 12,
        DirectiveLocation::Interface => 13,
        DirectiveLocation::Union => 14,
        DirectiveLocation::Enum => 15,
        DirectiveLocation::EnumValue => 16,
        DirectiveLocation::InputObject => 17,
        DirectiveLocation::InputFieldDefinition => 18,
    }
}

fn discriminant_value(value: &apollo_compiler::ast::Value) -> u8 {
    use apollo_compiler::ast::Value;
    match value {
        Value::Null => 0,
        Value::Enum(_) => 1,
        Value::Variable(_) => 2,
        Value::String(_) => 3,
        Value::Float(_) => 4,
        Value::Int(_) => 5,
        Value::Boolean(_) => 6,
        Value::List(_) => 7,
        Value::Object(_) => 8,
    }
}

fn discriminant_type(ty: &apollo_compiler::ast::Type) -> u8 {
    use apollo_compiler::ast::Type;
    match ty {
        Type::Named(_) => 0,
        Type::NonNullNamed(_) => 1,
        Type::List(_) => 2,
        Type::NonNullList(_) => 3,
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::schema::ExtendedType;

    use super::*;

    static TEST_SCHEMA: &str = r#"
    directive @d2(
      a2: I1
      a1: String
    ) repeatable on
      | FIELD_DEFINITION
      | SCALAR
      | ENUM
      | UNION
      | SUBSCRIPTION
      | ARGUMENT_DEFINITION
      | INTERFACE
      | FRAGMENT_DEFINITION
      | OBJECT
      | FIELD
      | INPUT_FIELD_DEFINITION
      | SCHEMA
      | INLINE_FRAGMENT
      | QUERY
      | INPUT_OBJECT
      | ENUM_VALUE

    directive @d1(
      a1: String
      a3: String
      a2: I2
    ) repeatable on
      | ENUM
      | INPUT_OBJECT
      | SCHEMA
      | INTERFACE
      | FRAGMENT_SPREAD
      | FIELD_DEFINITION
      | ENUM_VALUE
      | MUTATION
      | INPUT_FIELD_DEFINITION
      | OBJECT
      | VARIABLE_DEFINITION
      | SCALAR
      | ARGUMENT_DEFINITION
      | UNION

    extend schema @d1(a1: "0") {
      query: T3
    }

    extend schema @d2(a1: "0") @d1(a1: "2")

    schema @d1(a3: "2") {
      mutation: T1
    }

    extend schema @d1(a1: "1") @d2(a1: "0") @d1(a3: "2", a1: "0")

    extend schema {
      subscription: T2
    }

    extend schema @d2(a1: "0") @d1(a1: "2")

    extend input I1 {
      f1: String
      f3: [String!]
    }

    type T3 implements N1 & N2 @d2(a1: "1") {
      f2(
        a2: I1 @d1(a2: { f3: "ID2", f1: "ID1" }) @d1(a1: "ID0")
        a1: I2 @d1(a2: { f3: "ID0", f1: "ID2" }) @d1(a2: { f1: "ID1", f3: "ID1" })
      ): T1! @d2(a1: "1") @d1(a1: "1")
    }

    interface N2 @d2(a1: "1") @d1(a1: "2") {
      f3: ID!
    }

    input I2 {
      f2: I1
      f3: String
      f1: ID
    }

    extend type T3 implements N3 @d1(a1: "1") {
      f3: ID!
      f1(a1: Int): String
    }

    scalar S2

    type T1 {
      f2(a1: I2): E1
    }

    extend interface N2 implements N1 @d2(a1: "2") @d1(a1: "1") {
      f1(a1: Int): String
    }

    extend union U1 @d1 @d2 @d1 = T3

    input I1 {
      f2: I2
    }

    extend enum E1 @d1(a1: "0") @d1(a1: "0") {
      V3
      V1
    }

    union U1 @d2 @d1 @d2 = T1 | T2

    interface N3 {
      f2(a2: I1, a1: I2): T1
    }

    scalar S1 @d1(a3: "2")

    type T2 {
      f1: Float
    }

    interface N1 {
      f3: ID!
    }

    enum E1 @d1(a1: "0") @d1(a1: "0") {
      V2 @d2(a2: { f3: ["2", "1"] }) @d2(a2: { f3: ["1", "3"] })
    }
    "#;

    fn remove_extensions(schema: &mut Schema) {
        fn handle_component<T>(component: &mut Component<T>) {
            component.origin = ComponentOrigin::Definition
        }
        fn handle_component_name(component: &mut ComponentName) {
            component.origin = ComponentOrigin::Definition
        }
        fn handle_indexset(components: &mut IndexSet<ComponentName>) {
            *components = components
                .drain(..)
                .map(|mut component| {
                    handle_component_name(&mut component);
                    component
                })
                .collect();
        }
        let schema_definition = schema.schema_definition.make_mut();
        schema_definition
            .query
            .iter_mut()
            .for_each(handle_component_name);
        schema_definition
            .mutation
            .iter_mut()
            .for_each(handle_component_name);
        schema_definition
            .subscription
            .iter_mut()
            .for_each(handle_component_name);
        schema_definition
            .directives
            .iter_mut()
            .for_each(handle_component);
        schema
            .types
            .values_mut()
            .for_each(|definition| match definition {
                ExtendedType::Scalar(definition) => {
                    let definition = definition.make_mut();
                    definition.directives.iter_mut().for_each(handle_component);
                }
                ExtendedType::Object(definition) => {
                    let definition = definition.make_mut();
                    definition.fields.values_mut().for_each(handle_component);
                    handle_indexset(&mut definition.implements_interfaces);
                    definition.directives.iter_mut().for_each(handle_component);
                }
                ExtendedType::Interface(definition) => {
                    let definition = definition.make_mut();
                    definition.fields.values_mut().for_each(handle_component);
                    handle_indexset(&mut definition.implements_interfaces);
                    definition.directives.iter_mut().for_each(handle_component);
                }
                ExtendedType::Union(definition) => {
                    let definition = definition.make_mut();
                    handle_indexset(&mut definition.members);
                    definition.directives.iter_mut().for_each(handle_component);
                }
                ExtendedType::Enum(definition) => {
                    let definition = definition.make_mut();
                    definition.values.values_mut().for_each(handle_component);
                    definition.directives.iter_mut().for_each(handle_component);
                }
                ExtendedType::InputObject(definition) => {
                    let definition = definition.make_mut();
                    definition.fields.values_mut().for_each(handle_component);
                    definition.directives.iter_mut().for_each(handle_component);
                }
            })
    }

    #[test]
    fn test_round_trip_equality() {
        let schema = Schema::parse_and_validate(TEST_SCHEMA, "schema.graphql")
            .expect("Test schema unexpectedly invalid.");

        // Snapshot what the normalized schema should look like.
        let normalized_schema = normalize_valid_schema(schema);
        let normalized_sdl = normalized_schema.to_string();
        insta::assert_snapshot!(normalized_sdl);

        // Reparse the schema. Note that:
        // 1. At the time of writing this test, parsing-and-printing the normalized schema string
        //    results in the same schema string, but this isn't guaranteed in the future as it
        //    depends on parsing logic being aligned with printing logic in apollo-compiler, so we
        //    don't check that here.
        // 2. At the time of writing this test, the printed-and-parsed normalized Schema is not
        //    equal to the original normalized Schema. This is due to apollo-compiler parsing
        //    populating component containers in order of encountered definitions/extensions, while
        //    normalizing reorders component containers by their content. Since this parsing
        //    behavior may similarly change in the future, we similarly don't check that here.
        let reparsed_schema = Schema::parse_and_validate(&normalized_sdl, "schema.graphql")
            .expect("Reparsed test schema unexpectedly invalid.");

        // Renormalize the reparsed schema, and confirm it's fully equal.
        let normalized_reparsed_schema = normalize_valid_schema(reparsed_schema);
        assert_eq!(normalized_reparsed_schema, normalized_schema);
        assert_eq!(normalized_reparsed_schema.to_string(), normalized_sdl);
    }

    #[test]
    fn test_extension_equality() {
        let schema = Schema::parse_and_validate(TEST_SCHEMA, "schema.graphql")
            .expect("Test schema unexpectedly invalid.");

        // Normalize the schema with extensions.
        let normalized_schema = normalize_valid_schema(schema.clone());
        let normalized_sdl = normalized_schema.to_string();

        // Normalize the schema without extensions, and confirm it's still equal with PartialEq but
        // not with Display.
        let mut schema_no_exts = schema.into_inner();
        remove_extensions(&mut schema_no_exts);
        let schema_no_exts = schema_no_exts
            .validate()
            .expect("Test schema without extensions unexpectedly invalid.");
        let normalized_schema_no_exts = normalize_valid_schema(schema_no_exts);
        let normalized_sdl_no_exts = normalized_schema_no_exts.to_string();
        assert_eq!(normalized_schema_no_exts, normalized_schema);
        assert_ne!(normalized_sdl_no_exts, normalized_sdl);

        // Remove extensions from the original normalized schema, and confirm it's fully equal.
        let mut normalized_schema = normalized_schema.into_inner().into_inner();
        remove_extensions(&mut normalized_schema);
        let normalized_schema = normalized_schema
            .validate()
            .expect("Test schema without extensions unexpectedly invalid.");
        assert_eq!(&normalized_schema, normalized_schema_no_exts.as_ref());
        assert_eq!(normalized_schema.to_string(), normalized_sdl_no_exts);
    }
}
