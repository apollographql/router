#![allow(dead_code)]
// NOTE: There are several (technically) unused fields, type aliases, and methods in this module.
// Unfortunely, there is not a good way to clean this up because of how `` it is used for testing.
// Rather than littering this module with `#[allow(dead_code)]`s or adding a config_atr to the
// crate wide directive, allowing dead code here seems like the best options

use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::Value;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ComponentName;
use apollo_compiler::schema::DirectiveDefinition;
use apollo_compiler::schema::EnumType;
use apollo_compiler::schema::EnumValueDefinition;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::InputValueDefinition;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::schema::ScalarType;
use apollo_compiler::schema::Type;
use apollo_compiler::schema::UnionType;
use apollo_compiler::Name;
use apollo_compiler::Node;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::schema::argument_composition_strategies::ArgumentCompositionStrategy;
use crate::schema::position::DirectiveDefinitionPosition;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;
use crate::schema::position::UnionTypeDefinitionPosition;
use crate::schema::FederationSchema;

//////////////////////////////////////////////////////////////////////////////
// Field and Argument Specifications

/// Schema-dependent argument specification
#[derive(Clone)]
pub(crate) struct ArgumentSpecification {
    pub(crate) name: Name,
    // PORT_NOTE: In TS, get_type returns `InputType`.
    pub(crate) get_type: fn(schema: &FederationSchema) -> Result<Type, SingleFederationError>,
    pub(crate) default_value: Option<Value>,
}

/// The resolved version of `ArgumentSpecification`
pub(crate) struct ResolvedArgumentSpecification {
    pub(crate) name: Name,
    pub(crate) ty: Type,
    default_value: Option<Value>,
}

impl From<&ResolvedArgumentSpecification> for InputValueDefinition {
    fn from(arg_spec: &ResolvedArgumentSpecification) -> Self {
        InputValueDefinition {
            description: None,
            name: arg_spec.name.clone(),
            ty: Node::new(arg_spec.ty.clone()),
            default_value: arg_spec
                .default_value
                .as_ref()
                .map(|v| Node::new(v.clone())),
            directives: Default::default(),
        }
    }
}

pub(crate) struct FieldSpecification {
    pub(crate) name: Name,
    pub(crate) ty: Type,
    pub(crate) arguments: Vec<ResolvedArgumentSpecification>,
}

impl From<&FieldSpecification> for FieldDefinition {
    fn from(field_spec: &FieldSpecification) -> Self {
        FieldDefinition {
            description: None,
            name: field_spec.name.clone(),
            arguments: field_spec
                .arguments
                .iter()
                .map(|arg| Node::new(arg.into()))
                .collect(),
            ty: field_spec.ty.clone(),
            directives: Default::default(),
        }
    }
}

//////////////////////////////////////////////////////////////////////////////
// Type Specifications

pub(crate) trait TypeAndDirectiveSpecification {
    // PORT_NOTE: The JS version takes additional optional arguments `feature` and `asBuiltIn`.
    fn check_or_add(&self, schema: &mut FederationSchema) -> Result<(), FederationError>;
}

pub(crate) struct ScalarTypeSpecification {
    pub(crate) name: Name, // Type's name
}

impl TypeAndDirectiveSpecification for ScalarTypeSpecification {
    fn check_or_add(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let existing = schema.try_get_type(self.name.clone());
        if let Some(existing) = existing {
            // Ignore redundant type specifications if they are are both scalar types.
            return ensure_expected_type_kind(TypeKind::Scalar, &existing);
        }

        let type_pos = ScalarTypeDefinitionPosition {
            type_name: self.name.clone(),
        };
        type_pos.pre_insert(schema)?;
        type_pos.insert(
            schema,
            Node::new(ScalarType {
                description: None,
                name: type_pos.type_name.clone(),
                directives: Default::default(),
            }),
        )
    }
}

pub(crate) struct ObjectTypeSpecification {
    pub(crate) name: Name,
    pub(crate) fields: fn(&FederationSchema) -> Vec<FieldSpecification>,
}

impl TypeAndDirectiveSpecification for ObjectTypeSpecification {
    fn check_or_add(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let field_specs = (self.fields)(schema);
        let existing = schema.try_get_type(self.name.clone());
        if let Some(existing) = existing {
            // ensure existing definition is an object type
            ensure_expected_type_kind(TypeKind::Object, &existing)?;
            let existing_type = existing.get(schema.schema())?;
            let ExtendedType::Object(existing_obj_type) = existing_type else {
                return Err(FederationError::internal(format!(
                    "Expected ExtendedType::Object but got {}",
                    TypeKind::from(existing_type)
                )));
            };

            // ensure all expected fields are present in the existing object type
            let errors = ensure_same_fields(existing_obj_type, &field_specs, schema);
            return MultipleFederationErrors::from_iter(errors).into_result();
        }

        let mut field_map = IndexMap::default();
        for ref field_spec in field_specs {
            let field_def: FieldDefinition = field_spec.into();
            field_map.insert(field_spec.name.clone(), Component::new(field_def));
        }

        let type_pos = ObjectTypeDefinitionPosition {
            type_name: self.name.clone(),
        };
        type_pos.pre_insert(schema)?;
        type_pos.insert(
            schema,
            Node::new(ObjectType {
                description: None,
                name: type_pos.type_name.clone(),
                implements_interfaces: Default::default(),
                directives: Default::default(),
                fields: field_map,
            }),
        )
    }
}

pub(crate) struct UnionTypeSpecification<F>
where
    F: Fn(&FederationSchema) -> IndexSet<ComponentName>,
{
    pub(crate) name: Name,
    pub(crate) members: F,
}

impl<F> TypeAndDirectiveSpecification for UnionTypeSpecification<F>
where
    F: Fn(&FederationSchema) -> IndexSet<ComponentName>,
{
    fn check_or_add(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let members = (self.members)(schema);
        let existing = schema.try_get_type(self.name.clone());

        // ensure new union has at least one member
        if members.is_empty() {
            if existing.is_some() {
                let union_type_name = &self.name;
                return Err(SingleFederationError::TypeDefinitionInvalid {
                    message: format!("Invalid definition of type {union_type_name}: expected the union type to not exist/have no members but it is defined.")
                }.into());
            }
            return Ok(()); // silently ignore empty unions
        }

        // ensure new union has the same members as the existing union
        if let Some(existing) = existing {
            ensure_expected_type_kind(TypeKind::Union, &existing)?;
            let existing_type = existing.get(schema.schema())?;
            let ExtendedType::Union(existing_union_type) = existing_type else {
                return Err(FederationError::internal(format!(
                    "Expected ExtendedType::Object but got {}",
                    TypeKind::from(existing_type)
                )));
            };
            if existing_union_type.members != members {
                let union_type_name = &self.name;
                let expected_member_names: Vec<String> = existing_union_type
                    .members
                    .iter()
                    .map(|name| name.to_string())
                    .collect();
                let actual_member_names: Vec<String> =
                    members.iter().map(|name| name.to_string()).collect();
                return Err(SingleFederationError::TypeDefinitionInvalid {
                    message: format!("Invalid definition of type {union_type_name}: expected members [{}] but found [{}]",
                    expected_member_names.join(", "), actual_member_names.join(", "))
                }.into());
            }
            return Ok(());
        }

        let type_pos = UnionTypeDefinitionPosition {
            type_name: self.name.clone(),
        };
        type_pos.pre_insert(schema)?;
        type_pos.insert(
            schema,
            Node::new(UnionType {
                description: None,
                name: type_pos.type_name.clone(),
                directives: Default::default(),
                members,
            }),
        )
    }
}

pub(crate) struct EnumValueSpecification {
    pub(crate) name: Name,
    pub(crate) description: Option<String>,
}

pub(crate) struct EnumTypeSpecification {
    pub(crate) name: Name,
    pub(crate) values: Vec<EnumValueSpecification>,
}

impl TypeAndDirectiveSpecification for EnumTypeSpecification {
    fn check_or_add(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let existing = schema.try_get_type(self.name.clone());
        if let Some(existing) = existing {
            ensure_expected_type_kind(TypeKind::Enum, &existing)?;
            let existing_type = existing.get(schema.schema())?;
            let ExtendedType::Enum(existing_type) = existing_type else {
                return Err(FederationError::internal(format!(
                    "Expected ExtendedType::Union but got {}",
                    TypeKind::from(existing_type)
                )));
            };

            let existing_value_set: IndexSet<Name> = existing_type
                .values
                .iter()
                .map(|val| val.0.clone())
                .collect();
            let actual_value_set: IndexSet<Name> =
                self.values.iter().map(|val| val.name.clone()).collect();
            if existing_value_set != actual_value_set {
                let enum_type_name = &self.name;
                let expected_value_names: Vec<String> = existing_value_set
                    .iter()
                    .map(|name| name.to_string())
                    .collect();
                let actual_value_names: Vec<String> = actual_value_set
                    .iter()
                    .map(|name| name.to_string())
                    .collect();
                return Err(SingleFederationError::TypeDefinitionInvalid {
                    message: format!("Invalid definition of type {enum_type_name}: expected values [{}] but found [{}].",
                    expected_value_names.join(", "), actual_value_names.join(", "))
                }.into());
            }
            return Ok(());
        }

        let type_pos = EnumTypeDefinitionPosition {
            type_name: self.name.clone(),
        };
        type_pos.pre_insert(schema)?;
        type_pos.insert(
            schema,
            Node::new(EnumType {
                description: None,
                name: type_pos.type_name.clone(),
                directives: Default::default(),
                values: self
                    .values
                    .iter()
                    .map(|val| {
                        (
                            val.name.clone(),
                            Component::new(EnumValueDefinition {
                                description: val.description.as_ref().map(|s| s.into()),
                                value: val.name.clone(),
                                directives: Default::default(),
                            }),
                        )
                    })
                    .collect(),
            }),
        )
    }
}

//////////////////////////////////////////////////////////////////////////////
// DirectiveSpecification

#[derive(Clone)]
pub(crate) struct DirectiveArgumentSpecification {
    pub(crate) base_spec: ArgumentSpecification,
    pub(crate) composition_strategy: Option<ArgumentCompositionStrategy>,
}

type ArgumentMergerFn = dyn Fn(&str, &[Value]) -> Value;

pub(crate) struct ArgumentMerger {
    pub(crate) merge: Box<ArgumentMergerFn>,
    pub(crate) to_string: Box<dyn Fn() -> String>,
}

type ArgumentMergerFactory =
    dyn Fn(&FederationSchema) -> Result<ArgumentMerger, SingleFederationError>;

pub(crate) struct DirectiveCompositionSpecification {
    pub(crate) supergraph_specification: fn(
        federation_version: crate::link::spec::Version,
    ) -> Box<dyn crate::link::spec_definition::SpecDefinition>,
    /// Factory function returning an actual argument merger for given federation schema.
    pub(crate) argument_merger: Option<Box<ArgumentMergerFactory>>,
}

pub(crate) struct DirectiveSpecification {
    pub(crate) name: Name,
    pub(crate) composition: Option<DirectiveCompositionSpecification>,
    args: Vec<DirectiveArgumentSpecification>,
    repeatable: bool,
    locations: Vec<DirectiveLocation>,
}

// TODO: revisit DirectiveSpecification::new() API once we start porting
// composition.
// https://apollographql.atlassian.net/browse/FED-172
impl DirectiveSpecification {
    pub(crate) fn new(
        name: Name,
        args: &[DirectiveArgumentSpecification],
        repeatable: bool,
        locations: &[DirectiveLocation],
        composes: bool,
        supergraph_specification: Option<
            fn(
                federation_version: crate::link::spec::Version,
            ) -> Box<dyn crate::link::spec_definition::SpecDefinition>,
        >,
    ) -> Self {
        let mut composition: Option<DirectiveCompositionSpecification> = None;
        if composes {
            assert!( supergraph_specification.is_some(),
                "Should provide a @link specification to use in supergraph for directive @{name} if it composes");
            let mut argument_merger: Option<Box<ArgumentMergerFactory>> = None;
            let arg_strategies_iter = args
                .iter()
                .filter(|arg| arg.composition_strategy.is_some())
                .map(|arg| {
                    (
                        arg.base_spec.name.to_string(),
                        arg.composition_strategy.unwrap(),
                    )
                });
            let arg_strategies: IndexMap<String, ArgumentCompositionStrategy> =
                IndexMap::from_iter(arg_strategies_iter);
            if !arg_strategies.is_empty() {
                assert!(!repeatable, "Invalid directive specification for @{name}: @{name} is repeatable and should not define composition strategy for its arguments");
                assert!(arg_strategies.len() == args.len(), "Invalid directive specification for @{name}: not all arguments define a composition strategy");
                let name_capture = name.clone();
                let args_capture = args.to_vec();
                argument_merger = Some(Box::new(move |schema: &FederationSchema| -> Result<ArgumentMerger, SingleFederationError> {
                    for arg in args_capture.iter() {
                        let strategy = arg.composition_strategy.as_ref().unwrap();
                        let arg_name = &arg.base_spec.name;
                        let arg_type = (arg.base_spec.get_type)(schema)?;
                        assert!(!arg_type.is_list(), "Should have gotten error getting type for @{name_capture}({arg_name}:), but got {arg_type}");
                        strategy.is_type_supported(schema, &arg_type).map_err(|support_msg| {
                            let strategy_name = strategy.name();
                            SingleFederationError::DirectiveDefinitionInvalid {
                                message: format!("Invalid composition strategy {strategy_name} for argument @{name_capture}({arg_name}:) of type {arg_type}; {strategy_name} only supports ${support_msg}")
                            }
                        })?;
                    }
                    let arg_strategies_capture = arg_strategies.clone();
                    let arg_strategies_capture2 = arg_strategies.clone();
                    Ok(ArgumentMerger {
                        merge: Box::new(move |arg_name: &str, values: &[Value]| {
                            let Some(strategy) = arg_strategies_capture.get(arg_name) else {
                                panic!("`Should have a strategy for {arg_name}")
                            };
                            strategy.merge_values(values)
                        }),
                        to_string: Box::new(move || {
                            if arg_strategies_capture2.is_empty() {
                                "<none>".to_string()
                            }
                            else {
                                let arg_strategy_strings: Vec<String> = arg_strategies_capture2
                                    .iter()
                                    .map(|(arg_name, strategy)| format!("{arg_name}: {}", strategy.name()))
                                    .collect();
                                format!("{{ {} }}", arg_strategy_strings.join(", "))
                            }
                        }),
                    })
                }));
            }
            composition = Some(DirectiveCompositionSpecification {
                supergraph_specification: supergraph_specification.unwrap(),
                argument_merger,
            })
        }
        Self {
            name,
            composition,
            args: args.to_vec(),
            repeatable,
            locations: locations.to_vec(),
        }
    }
}

impl TypeAndDirectiveSpecification for DirectiveSpecification {
    fn check_or_add(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let mut resolved_args = Vec::new();
        let mut errors = MultipleFederationErrors { errors: vec![] };
        for arg in self.args.iter() {
            match (arg.base_spec.get_type)(schema) {
                Ok(arg_type) => {
                    resolved_args.push(ResolvedArgumentSpecification {
                        name: arg.base_spec.name.clone(),
                        ty: arg_type,
                        default_value: arg.base_spec.default_value.clone(),
                    });
                }
                Err(err) => {
                    errors.errors.push(err);
                }
            };
        }
        errors.into_result()?;
        let existing = schema.get_directive_definition(&self.name);
        if let Some(existing) = existing {
            let existing_directive = existing.get(schema.schema())?;
            return ensure_same_directive_structure(
                existing_directive,
                &self.name,
                &resolved_args,
                self.repeatable,
                &self.locations,
                schema,
            );
        }

        let directive_pos = DirectiveDefinitionPosition {
            directive_name: self.name.clone(),
        };
        directive_pos.pre_insert(schema)?;
        directive_pos.insert(
            schema,
            Node::new(DirectiveDefinition {
                description: None,
                name: self.name.clone(),
                arguments: resolved_args
                    .iter()
                    .map(|arg| Node::new(arg.into()))
                    .collect(),
                repeatable: self.repeatable,
                locations: self.locations.clone(),
            }),
        )
    }
}

//////////////////////////////////////////////////////////////////////////////
// Helper functions for TypeSpecification implementations

// TODO: Consider moving this to the schema module.
#[derive(Clone, PartialEq, Eq, Hash, derive_more::Display)]
pub(crate) enum TypeKind {
    Scalar,
    Object,
    Interface,
    Union,
    Enum,
    InputObject,
}

impl From<&ExtendedType> for TypeKind {
    fn from(value: &ExtendedType) -> Self {
        match value {
            ExtendedType::Scalar(_) => TypeKind::Scalar,
            ExtendedType::Object(_) => TypeKind::Object,
            ExtendedType::Interface(_) => TypeKind::Interface,
            ExtendedType::Union(_) => TypeKind::Union,
            ExtendedType::Enum(_) => TypeKind::Enum,
            ExtendedType::InputObject(_) => TypeKind::InputObject,
        }
    }
}

impl From<&TypeDefinitionPosition> for TypeKind {
    fn from(value: &TypeDefinitionPosition) -> Self {
        match value {
            TypeDefinitionPosition::Scalar(_) => TypeKind::Scalar,
            TypeDefinitionPosition::Object(_) => TypeKind::Object,
            TypeDefinitionPosition::Interface(_) => TypeKind::Interface,
            TypeDefinitionPosition::Union(_) => TypeKind::Union,
            TypeDefinitionPosition::Enum(_) => TypeKind::Enum,
            TypeDefinitionPosition::InputObject(_) => TypeKind::InputObject,
        }
    }
}

fn ensure_expected_type_kind(
    expected: TypeKind,
    actual: &TypeDefinitionPosition,
) -> Result<(), FederationError> {
    let actual_kind: TypeKind = TypeKind::from(actual);
    if expected == actual_kind {
        Ok(())
    } else {
        let actual_type_name = actual.type_name();
        Err(SingleFederationError::TypeDefinitionInvalid {
            message: format!("Invalid definition for type {actual_type_name}: {actual_type_name} should be a {expected} but is defined as a {actual_kind}")
        }.into())
    }
}

/// Note: Non-null/list wrappers are ignored.
fn is_custom_scalar(ty: &Type, schema: &FederationSchema) -> bool {
    let type_name = ty.inner_named_type().as_str();
    schema
        .schema()
        .get_scalar(type_name)
        .is_some_and(|scalar| !scalar.is_built_in())
}

fn is_valid_input_type_redefinition(
    expected_type: &Type,
    actual_type: &Type,
    schema: &FederationSchema,
) -> bool {
    // If the expected type is a custom scalar, then we allow the redefinition to be another type (unless it's a custom scalar, in which
    // case it has to be the same scalar). The rational being that since graphQL does no validation of values passed to a custom scalar,
    // any code that gets some value as input for a custom scalar has to do validation manually, and so there is little harm in allowing
    // a redefinition with another type since any truly invalid value would failed that "manual validation". In practice, this leeway
    // make sense because many scalar will tend to accept only one kind of values (say, strings) and exists only to inform that said string
    // needs to follow a specific format, and in such case, letting user redefine the type as String adds flexibility while doing little harm.
    if expected_type.is_list() {
        return actual_type.is_list()
            && is_valid_input_type_redefinition(
                expected_type.item_type(),
                actual_type.item_type(),
                schema,
            );
    }
    if expected_type.is_non_null() {
        return actual_type.is_non_null()
            && is_valid_input_type_redefinition(
                &expected_type.clone().nullable(),
                &actual_type.clone().nullable(),
                schema,
            );
    }
    // invariant: expected_type/actual_type is not a list or a non-null type (thus a named type).
    is_custom_scalar(expected_type, schema) && !is_custom_scalar(actual_type, schema)
}

fn default_value_message(value: Option<&Value>) -> String {
    match value {
        None => "no default value".to_string(),
        Some(value) => format!("default value {}", value),
    }
}

fn ensure_same_arguments(
    expected: &[Node<InputValueDefinition>],
    actual: &[ResolvedArgumentSpecification],
    schema: &FederationSchema,
    what: &str,
    generate_error: fn(&str) -> SingleFederationError,
) -> Vec<SingleFederationError> {
    let mut errors = vec![];

    // ensure expected arguments are a subset of actual arguments.
    for expected_arg in expected {
        let actual_arg = actual.iter().find(|x| x.name == expected_arg.name);
        if actual_arg.is_none() {
            // Not declaring an optional argument is ok: that means you won't be able to pass a non-default value in your schema, but we allow you that.
            // But missing a required argument it not ok.
            if expected_arg.ty.is_non_null() && expected_arg.default_value.is_none() {
                let expected_arg_name = &expected_arg.name;
                errors.push(generate_error(&format!(
                        r#"Invalid definition for {what}: Missing required argument "{expected_arg_name}""#
                    )));
            }
            continue;
        }

        // ensure expected argument and actual argument have the same type.
        let actual_arg = actual_arg.unwrap();
        // TODO: Make it easy to get a cloned (inner) type from a Node<Type>.
        let mut actual_type = actual_arg.ty.clone();
        if actual_type.is_non_null() && !expected_arg.ty.is_non_null() {
            // It's ok to redefine an optional argument as mandatory. For instance, if you want to force people on your team to provide a "deprecation reason", you can
            // redefine @deprecated as `directive @deprecated(reason: String!)...` to get validation. In other words, you are allowed to always pass an argument that
            // is optional if you so wish.
            actual_type = actual_type.nullable();
        }
        // ensure argument type is compatible with the expected one and
        // argument's default value (if any) is compatible with the expected one
        if *expected_arg.ty != actual_type
            && is_valid_input_type_redefinition(&expected_arg.ty, &actual_type, schema)
        {
            let arg_name = &expected_arg.name;
            let expected_type = &expected_arg.ty;
            errors.push(generate_error(&format!(
                    r#"Invalid definition for {what}: Argument "{arg_name}" should have type {expected_type} but found type {actual_type}"#
                )));
        } else if !actual_type.is_non_null()
            && expected_arg.default_value.as_deref() != actual_arg.default_value.as_ref()
        {
            let arg_name = &expected_arg.name;
            let expected_value = default_value_message(expected_arg.default_value.as_deref());
            let actual_value = default_value_message(actual_arg.default_value.as_ref());
            errors.push(generate_error(&format!(
                    r#"Invalid definition for {what}: Argument "{arg_name}" should have {expected_value} but found {actual_value}"#
                )));
        }
    }

    // ensure actual arguments are a subset of expected arguments.
    for actual_arg in actual {
        let expected_arg = expected.iter().find(|x| x.name == actual_arg.name);
        if expected_arg.is_none() {
            let arg_name = &actual_arg.name;
            errors.push(generate_error(&format!(
                r#"Invalid definition for {what}: unknown/unsupported argument "{arg_name}""#
            )));
            // fall through to the next iteration
        }
    }

    errors
}

fn ensure_same_fields(
    existing_obj_type: &ObjectType,
    actual_fields: &[FieldSpecification],
    schema: &FederationSchema,
) -> Vec<SingleFederationError> {
    let obj_type_name = existing_obj_type.name.clone();
    let mut errors = vec![];

    // ensure all actual fields are a subset of the existing object type's fields.
    for actual_field_def in actual_fields {
        let actual_field_name = &actual_field_def.name;
        let expected_field = existing_obj_type.fields.get(actual_field_name);
        if expected_field.is_none() {
            errors.push(SingleFederationError::TypeDefinitionInvalid {
                message: format!(
                    "Invalid definition of type {}: missing field {}",
                    obj_type_name, actual_field_name
                ),
            });
            continue;
        }

        // ensure field types are as expected
        let expected_field = expected_field.unwrap();
        if actual_field_def.ty != expected_field.ty {
            let expected_field_type = &expected_field.ty;
            let actual_field_type = &actual_field_def.ty;
            errors.push(SingleFederationError::TypeDefinitionInvalid {
                message: format!("Invalid definition for field {actual_field_name} of type {obj_type_name}: should have type {expected_field_type} but found type {actual_field_type}")
            });
        }

        // ensure field arguments are as expected
        let mut arg_errors = ensure_same_arguments(
            &expected_field.arguments,
            &actual_field_def.arguments,
            schema,
            &format!(r#"field "{}.{}""#, obj_type_name, expected_field.name),
            |s| SingleFederationError::TypeDefinitionInvalid {
                message: s.to_string(),
            },
        );
        errors.append(&mut arg_errors);
    }

    errors
}

fn ensure_same_directive_structure(
    existing_directive: &DirectiveDefinition,
    name: &Name,
    args: &[ResolvedArgumentSpecification],
    repeatable: bool,
    locations: &[DirectiveLocation],
    schema: &FederationSchema,
) -> Result<(), FederationError> {
    let directive_name = format!("@{name}");
    let mut arg_errors = ensure_same_arguments(
        &existing_directive.arguments,
        args,
        schema,
        &format!(r#"directive {directive_name}"#),
        |s| SingleFederationError::DirectiveDefinitionInvalid {
            message: s.to_string(),
        },
    );

    // It's ok to say you'll never repeat a repeatable directive. It's not ok to repeat one that isn't.
    if !existing_directive.repeatable && repeatable {
        arg_errors.push(SingleFederationError::DirectiveDefinitionInvalid {
            message: format!(
                "Invalid definition for directive {directive_name}: {directive_name} should not be repeatable"
            ),
        });
    }

    // Similarly, it's ok to say that you will never use a directive in some locations, but not that
    // you will use it in places not allowed by what is expected.
    // Ensure `locations` is a subset of `existing_directive.locations`.
    if !locations
        .iter()
        .all(|loc| existing_directive.locations.contains(loc))
    {
        let actual_locations: Vec<String> = locations.iter().map(|loc| loc.to_string()).collect();
        let existing_locations: Vec<String> = existing_directive
            .locations
            .iter()
            .map(|loc| loc.to_string())
            .collect();
        arg_errors.push(SingleFederationError::DirectiveDefinitionInvalid {
            message: format!(
                "Invalid definition for directive {directive_name}: {directive_name} should have locations [{}] but found [{}]",
                existing_locations.join(", "), actual_locations.join(", ")
            ),
        });
    }
    MultipleFederationErrors::from_iter(arg_errors).into_result()
}

#[cfg(test)]
mod tests {
    use apollo_compiler::ast::DirectiveLocation;
    use apollo_compiler::ast::Type;
    use apollo_compiler::name;

    use super::ArgumentSpecification;
    use super::DirectiveArgumentSpecification;
    use crate::error::SingleFederationError;
    use crate::link::link_spec_definition::LinkSpecDefinition;
    use crate::link::spec::Identity;
    use crate::link::spec::Version;
    use crate::link::spec_definition::SpecDefinition;
    use crate::schema::argument_composition_strategies::ArgumentCompositionStrategy;
    use crate::schema::type_and_directive_specification::DirectiveSpecification;
    use crate::schema::FederationSchema;

    #[test]
    fn dead_code_filter() {}

    #[test]
    #[should_panic(
        expected = "Should provide a @link specification to use in supergraph for directive @foo if it composes"
    )]
    fn must_have_supergraph_link_if_composed() {
        DirectiveSpecification::new(
            name!("foo"),
            &[],
            false,
            &[DirectiveLocation::Object],
            true,
            None,
        );
    }

    #[test]
    #[should_panic(
        expected = "Invalid directive specification for @foo: not all arguments define a composition strategy"
    )]
    fn must_have_a_merge_strategy_on_all_arguments_if_any() {
        fn link_spec(_version: Version) -> Box<dyn SpecDefinition> {
            Box::new(LinkSpecDefinition::new(
                Version { major: 1, minor: 0 },
                None,
                Identity {
                    domain: String::from("https://specs.apollo.dev/link/v1.0"),
                    name: name!("link"),
                },
            ))
        }

        DirectiveSpecification::new(
            name!("foo"),
            &[
                DirectiveArgumentSpecification {
                    base_spec: ArgumentSpecification {
                        name: name!("v1"),
                        get_type:
                            move |_schema: &FederationSchema| -> Result<Type, SingleFederationError> {
                                Ok(Type::Named(name!("Int")))
                            },
                        default_value: None,
                    },
                    composition_strategy: Some(ArgumentCompositionStrategy::Max),
                },
                DirectiveArgumentSpecification {
                    base_spec: ArgumentSpecification {
                        name: name!("v2"),
                        get_type:
                            move |_schema: &FederationSchema| -> Result<Type, SingleFederationError> {
                                Ok(Type::Named(name!("Int")))
                            },
                        default_value: None,
                    },
                    composition_strategy: None,
                },
            ],
            false,
            &[DirectiveLocation::Object],
            true,
            Some(link_spec)
        );
    }

    #[test]
    #[should_panic(
        expected = "Invalid directive specification for @foo: @foo is repeatable and should not define composition strategy for its arguments"
    )]
    fn must_be_not_be_repeatable_if_it_has_a_merge_strategy() {
        fn link_spec(_version: Version) -> Box<dyn SpecDefinition> {
            Box::new(LinkSpecDefinition::new(
                Version { major: 1, minor: 0 },
                None,
                Identity {
                    domain: String::from("https://specs.apollo.dev/link/v1.0"),
                    name: name!("link"),
                },
            ))
        }

        DirectiveSpecification::new(
            name!("foo"),
            &[DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: name!("v"),
                    get_type:
                        move |_schema: &FederationSchema| -> Result<Type, SingleFederationError> {
                            Ok(Type::Named(name!("Int")))
                        },
                    default_value: None,
                },
                composition_strategy: Some(ArgumentCompositionStrategy::Max),
            }],
            true,
            &[DirectiveLocation::Object],
            true,
            Some(link_spec),
        );
    }
}
