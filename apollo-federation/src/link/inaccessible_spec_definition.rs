use std::fmt;

use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ComponentName;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::DirectiveDefinition;
use apollo_compiler::schema::DirectiveLocation;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::FieldDefinition;
use apollo_compiler::schema::InputValueDefinition;
use apollo_compiler::schema::Name;
use apollo_compiler::schema::Value;
use apollo_compiler::Node;
use indexmap::IndexMap;
use indexmap::IndexSet;
use lazy_static::lazy_static;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::position;
use crate::schema::position::DirectiveDefinitionPosition;
use crate::schema::position::EnumValueDefinitionPosition;
use crate::schema::position::InputObjectFieldDefinitionPosition;
use crate::schema::position::InterfaceFieldArgumentDefinitionPosition;
use crate::schema::position::InterfaceFieldDefinitionPosition;
use crate::schema::position::ObjectFieldArgumentDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;
use crate::schema::position::TypeDefinitionPosition;
use crate::schema::FederationSchema;

pub(crate) const INACCESSIBLE_DIRECTIVE_NAME_IN_SPEC: Name = name!("inaccessible");

pub(crate) struct InaccessibleSpecDefinition {
    url: Url,
    minimum_federation_version: Option<Version>,
}

impl InaccessibleSpecDefinition {
    pub(crate) fn new(version: Version, minimum_federation_version: Option<Version>) -> Self {
        Self {
            url: Url {
                identity: Identity::inaccessible_identity(),
                version,
            },
            minimum_federation_version,
        }
    }

    pub(crate) fn inaccessible_directive(
        &self,
        schema: &FederationSchema,
    ) -> Result<Directive, FederationError> {
        let name_in_schema = self
            .directive_name_in_schema(schema, &INACCESSIBLE_DIRECTIVE_NAME_IN_SPEC)?
            .ok_or_else(|| SingleFederationError::Internal {
                message: "Unexpectedly could not find inaccessible spec in schema".to_owned(),
            })?;
        Ok(Directive {
            name: name_in_schema,
            arguments: Vec::new(),
        })
    }

    /// Returns the `@inaccessible` spec used in the given schema, if any.
    ///
    /// # Errors
    /// Returns an error if the schema specifies an `@inaccessible` spec version that is not
    /// supported by this version of the apollo-federation crate.
    pub fn get_from_schema(
        schema: &FederationSchema,
    ) -> Result<Option<&'static Self>, FederationError> {
        let inaccessible_link = match schema
            .metadata()
            .as_ref()
            .and_then(|metadata| metadata.for_identity(&Identity::inaccessible_identity()))
        {
            None => return Ok(None),
            Some(link) => link,
        };
        Ok(Some(
            INACCESSIBLE_VERSIONS
                .find(&inaccessible_link.url.version)
                .ok_or_else(|| SingleFederationError::Internal {
                    message: format!("Cannot remove inaccessible elements: the schema uses unsupported inaccessible spec version {}", inaccessible_link.url.version),
                })?,
        ))
    }

    pub fn validate_inaccessible(&self, schema: &FederationSchema) -> Result<(), FederationError> {
        validate_inaccessible(schema, self)
    }

    pub fn remove_inaccessible_elements(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        remove_inaccessible_elements(schema, self)
    }
}

impl SpecDefinition for InaccessibleSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn minimum_federation_version(&self) -> Option<&Version> {
        self.minimum_federation_version.as_ref()
    }
}

lazy_static! {
    pub(crate) static ref INACCESSIBLE_VERSIONS: SpecDefinitions<InaccessibleSpecDefinition> = {
        let mut definitions = SpecDefinitions::new(Identity::inaccessible_identity());
        definitions.add(InaccessibleSpecDefinition::new(
            Version { major: 0, minor: 1 },
            None,
        ));
        definitions.add(InaccessibleSpecDefinition::new(
            Version { major: 0, minor: 2 },
            Some(Version { major: 2, minor: 0 }),
        ));
        definitions
    };
}

fn is_type_system_location(location: DirectiveLocation) -> bool {
    matches!(
        location,
        DirectiveLocation::Schema
            | DirectiveLocation::Scalar
            | DirectiveLocation::Object
            | DirectiveLocation::FieldDefinition
            | DirectiveLocation::ArgumentDefinition
            | DirectiveLocation::Interface
            | DirectiveLocation::Union
            | DirectiveLocation::Enum
            | DirectiveLocation::EnumValue
            | DirectiveLocation::InputObject
            | DirectiveLocation::InputFieldDefinition
    )
}

fn field_uses_inaccessible(field: &FieldDefinition, inaccessible_directive: &Name) -> bool {
    field.directives.has(inaccessible_directive)
        || field
            .arguments
            .iter()
            .any(|argument| argument.directives.has(inaccessible_directive))
}

/// Check if a type definition uses the @inaccessible directive anywhere in its definition.
fn type_uses_inaccessible(
    schema: &FederationSchema,
    inaccessible_directive: &Name,
    position: &TypeDefinitionPosition,
) -> Result<bool, FederationError> {
    Ok(match position {
        TypeDefinitionPosition::Scalar(scalar_position) => {
            let scalar = scalar_position.get(schema.schema())?;
            scalar.directives.has(inaccessible_directive)
        }
        TypeDefinitionPosition::Object(object_position) => {
            let object = object_position.get(schema.schema())?;
            object.directives.has(inaccessible_directive)
                || object
                    .fields
                    .values()
                    .any(|field| field_uses_inaccessible(field, inaccessible_directive))
        }
        TypeDefinitionPosition::Interface(interface_position) => {
            let interface = interface_position.get(schema.schema())?;
            interface.directives.has(inaccessible_directive)
                || interface
                    .fields
                    .values()
                    .any(|field| field_uses_inaccessible(field, inaccessible_directive))
        }
        TypeDefinitionPosition::Union(union_position) => {
            let union_ = union_position.get(schema.schema())?;
            union_.directives.has(inaccessible_directive)
        }
        TypeDefinitionPosition::Enum(enum_position) => {
            let enum_ = enum_position.get(schema.schema())?;
            enum_.directives.has(inaccessible_directive)
                || enum_
                    .values
                    .values()
                    .any(|value| value.directives.has(inaccessible_directive))
        }
        TypeDefinitionPosition::InputObject(input_object_position) => {
            let input_object = input_object_position.get(schema.schema())?;
            input_object.directives.has(inaccessible_directive)
                || input_object
                    .fields
                    .values()
                    .any(|field| field.directives.has(inaccessible_directive))
        }
    })
}

/// Check if a directive uses the @inaccessible directive anywhere in its definition.
fn directive_uses_inaccessible(
    inaccessible_directive: &Name,
    directive: &DirectiveDefinition,
) -> bool {
    directive
        .arguments
        .iter()
        .any(|argument| argument.directives.has(inaccessible_directive))
}

enum HasArgumentDefinitionsPosition {
    ObjectField(ObjectFieldDefinitionPosition),
    InterfaceField(InterfaceFieldDefinitionPosition),
    DirectiveDefinition(DirectiveDefinitionPosition),
}
impl fmt::Display for HasArgumentDefinitionsPosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ObjectField(x) => x.fmt(f),
            Self::InterfaceField(x) => x.fmt(f),
            Self::DirectiveDefinition(x) => x.fmt(f),
        }
    }
}

fn validate_inaccessible_in_default_value(
    schema: &FederationSchema,
    inaccessible_directive: &Name,
    value_type: &ExtendedType,
    default_value: &Value,
    // TODO avoid eagerly stringifying this
    value_position: String,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    match (default_value, value_type) {
        // Input fields can be referenced by schema default values. When an
        // input field is hidden (but its parent isn't), we check that the
        // arguments/input fields with such default values aren't in the API
        // schema.
        (Value::Object(value), ExtendedType::InputObject(type_)) => {
            for (field_name, child_value) in value {
                let Some(field) = type_.fields.get(field_name) else {
                    return Ok(());
                };
                let input_field_position = InputObjectFieldDefinitionPosition {
                    type_name: type_.name.clone(),
                    field_name: field_name.clone(),
                };
                if input_field_position.is_inaccessible(schema, inaccessible_directive)? {
                    errors.push(SingleFederationError::DefaultValueUsesInaccessible {
                        message: format!("Input field `{input_field_position}` is @inaccessible but is used in the default value of `{value_position}`, which is in the API schema."),
                    }.into());
                }

                if let Some(field_type) = schema.schema().types.get(field.ty.inner_named_type()) {
                    validate_inaccessible_in_default_value(
                        schema,
                        inaccessible_directive,
                        field_type,
                        child_value,
                        value_position.clone(),
                        errors,
                    )?;
                }
            }
        }
        (Value::List(list), _) => {
            for child_value in list {
                validate_inaccessible_in_default_value(
                    schema,
                    inaccessible_directive,
                    value_type,
                    child_value,
                    value_position.clone(),
                    errors,
                )?;
            }
        }
        // Enum values can be referenced by schema default values. When an
        // enum value is hidden (but its parent isn't), we check that the
        // arguments/input fields with such default values aren't in the API
        // schema.
        //
        // For back-compat, this also supports using string literals where an enum value is
        // expected.
        (Value::Enum(_) | Value::String(_), ExtendedType::Enum(type_)) => {
            let value = match default_value {
                Value::Enum(name) => name.clone(),
                // It's no problem if this name is invalid.
                Value::String(node_str) => Name::new_unchecked(node_str.clone()),
                // Guaranteed to be enum or string by parent match branch.
                _ => unreachable!(),
            };
            let Some(enum_value) = type_.values.get(&value) else {
                return Ok(());
            };
            let enum_value_position = EnumValueDefinitionPosition {
                type_name: type_.name.clone(),
                value_name: enum_value.value.clone(),
            };
            if enum_value_position.is_inaccessible(schema, inaccessible_directive)? {
                errors.push(SingleFederationError::DefaultValueUsesInaccessible {
                    message: format!("Enum value `{enum_value_position}` is @inaccessible but is used in the default value of `{value_position}`, which is in the API schema."),
                }.into());
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_inaccessible_in_arguments(
    schema: &FederationSchema,
    inaccessible_directive: &Name,
    usage_position: HasArgumentDefinitionsPosition,
    arguments: &Vec<Node<InputValueDefinition>>,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let types = &schema.schema().types;
    for arg in arguments {
        let arg_name = &arg.name;
        let arg_inaccessible = arg.directives.has(inaccessible_directive);

        // When an argument is hidden (but its parent isn't), we check that it
        // isn't a required argument.
        if arg_inaccessible && arg.is_required() {
            let kind = match usage_position {
                HasArgumentDefinitionsPosition::DirectiveDefinition(_) => "directive",
                _ => "field",
            };
            errors.push(SingleFederationError::RequiredInaccessible {
                message: format!("Argument `{usage_position}({arg_name}:)` is @inaccessible but is a required argument of its {kind}."),
            }.into());
        }

        if !arg_inaccessible {
            if let (Some(default_value), Some(arg_type)) =
                (&arg.default_value, types.get(arg.ty.inner_named_type()))
            {
                validate_inaccessible_in_default_value(
                    schema,
                    inaccessible_directive,
                    arg_type,
                    default_value,
                    format!("{usage_position}({arg_name}:)"),
                    errors,
                )?;
            }
        }
    }
    Ok(())
}

fn validate_inaccessible_in_fields(
    schema: &FederationSchema,
    inaccessible_directive: &Name,
    type_position: &TypeDefinitionPosition,
    fields: &IndexMap<Name, Component<FieldDefinition>>,
    implements: &IndexSet<ComponentName>,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let mut has_inaccessible_field = false;
    let mut has_accessible_field = false;
    for (field_name, field) in fields {
        let field_inaccessible = field.directives.has(inaccessible_directive);
        has_inaccessible_field |= field_inaccessible;
        has_accessible_field |= !field_inaccessible;
        if field_inaccessible {
            // Fields can be "referenced" by the corresponding fields of any
            // interfaces their parent type implements. When a field is hidden
            // (but its parent isn't), we check that such implemented fields
            // aren't in the API schema.
            let accessible_super_references = implements.iter().filter_map(|interface_name| {
                let super_type = schema.schema().get_interface(interface_name)?;
                if super_type.directives.has(inaccessible_directive) {
                    return None;
                }
                let super_field = super_type.fields.get(field_name)?;
                if super_field.directives.has(inaccessible_directive) {
                    return None;
                }
                Some(InterfaceFieldDefinitionPosition {
                    type_name: super_type.name.clone(),
                    field_name: super_field.name.clone(),
                })
            });

            for super_position in accessible_super_references {
                errors.push(
                    SingleFederationError::ImplementedByInaccessible {
                        message: format!("Field `{type_position}.{field_name}` is @inaccessible but implements the interface field `{super_position}`, which is in the API schema."),
                    }
                    .into(),
                );
            }
        } else {
            // Arguments can be "referenced" by the corresponding arguments
            // of any interfaces their parent type implements. When an
            // argument is hidden (but its ancestors aren't), we check that
            // such implemented arguments aren't in the API schema.
            for arg in &field.arguments {
                let arg_name = &arg.name;
                let arg_inaccessible = arg.directives.has(inaccessible_directive);

                let accessible_super_references = implements.iter().filter_map(|interface_name| {
                    let super_type = schema.schema().get_interface(interface_name)?;
                    if super_type.directives.has(inaccessible_directive) {
                        return None;
                    }
                    let super_field = super_type.fields.get(field_name)?;
                    if super_field.directives.has(inaccessible_directive) {
                        return None;
                    }
                    let super_argument = super_field.argument_by_name(arg_name)?;
                    if super_argument.directives.has(inaccessible_directive) {
                        return None;
                    }
                    Some(InterfaceFieldArgumentDefinitionPosition {
                        type_name: super_type.name.clone(),
                        field_name: super_field.name.clone(),
                        argument_name: super_argument.name.clone(),
                    })
                });

                if arg_inaccessible {
                    for accessible_reference in accessible_super_references {
                        errors.push(SingleFederationError::ImplementedByInaccessible {
                            message: format!("Argument `{type_position}.{field_name}({arg_name}:)` is @inaccessible but implements the interface argument `{accessible_reference}` which is in the API schema."),
                        }.into());
                    }
                } else if arg.is_required() {
                    // When an argument is accessible and required, we check that
                    // it isn't marked inaccessible in any interface implemented by
                    // the argument's field. This is because the GraphQL spec
                    // requires that any arguments of an implementing field that
                    // aren't in its implemented field are optional.
                    //
                    // You might be thinking that a required argument in an
                    // implementing field would necessitate that the implemented
                    // field would also require that argument (and thus the check
                    // in `validate_inaccessible_in_arguments` would also always
                    // error, removing the need for this one), but the GraphQL spec
                    // does not enforce this. E.g. it's valid GraphQL for the
                    // implementing and implemented arguments to be both
                    // non-nullable, but for just the implemented argument to have
                    // a default value. Not providing a value for the argument when
                    // querying the implemented type succeeds GraphQL operation
                    // validation, but results in input coercion failure for the
                    // field at runtime.
                    let inaccessible_super_references =
                        implements.iter().filter_map(|interface_name| {
                            let super_type = schema.schema().get_interface(interface_name)?;
                            if super_type.directives.has(inaccessible_directive) {
                                return None;
                            }
                            let super_field = super_type.fields.get(field_name)?;
                            if super_field.directives.has(inaccessible_directive) {
                                return None;
                            }
                            let super_argument = super_field.argument_by_name(arg_name)?;
                            if !super_argument.directives.has(inaccessible_directive) {
                                return None;
                            }
                            Some(InterfaceFieldArgumentDefinitionPosition {
                                type_name: super_type.name.clone(),
                                field_name: super_field.name.clone(),
                                argument_name: super_argument.name.clone(),
                            })
                        });

                    for inaccessible_reference in inaccessible_super_references {
                        errors.push(SingleFederationError::RequiredInaccessible {
                            message: format!("Argument `{inaccessible_reference}` is @inaccessible but is implemented by the argument `{type_position}.{field_name}({arg_name}:)` which is in the API schema."),
                        }.into());
                    }
                }
            }
            validate_inaccessible_in_arguments(
                schema,
                inaccessible_directive,
                match type_position {
                    TypeDefinitionPosition::Object(object) => {
                        HasArgumentDefinitionsPosition::ObjectField(
                            object.field(field.name.clone()),
                        )
                    }
                    TypeDefinitionPosition::Interface(interface) => {
                        HasArgumentDefinitionsPosition::InterfaceField(
                            interface.field(field.name.clone()),
                        )
                    }
                    _ => unreachable!(),
                },
                &field.arguments,
                errors,
            )?;
        }
    }

    if has_inaccessible_field && !has_accessible_field {
        errors.push(SingleFederationError::OnlyInaccessibleChildren {
            message: format!("Type `{type_position}` is in the API schema but all of its members are @inaccessible."),
        }.into());
    }

    Ok(())
}

/// Generic way to check for @inaccessible directives on a position or its parents.
trait IsInaccessibleExt {
    /// Does this element, or any of its parents, have an @inaccessible directive?
    ///
    /// May return Err if `self` is an element that does not exist in the schema.
    fn is_inaccessible(
        &self,
        schema: &FederationSchema,
        inaccessible_directive: &Name,
    ) -> Result<bool, FederationError>;
}
impl IsInaccessibleExt for position::ObjectTypeDefinitionPosition {
    fn is_inaccessible(
        &self,
        schema: &FederationSchema,
        inaccessible_directive: &Name,
    ) -> Result<bool, FederationError> {
        let object = self.get(schema.schema())?;
        Ok(object.directives.has(inaccessible_directive))
    }
}
impl IsInaccessibleExt for position::ObjectFieldDefinitionPosition {
    fn is_inaccessible(
        &self,
        schema: &FederationSchema,
        inaccessible_directive: &Name,
    ) -> Result<bool, FederationError> {
        // NOTE It'd be more efficient to start at parent and look up the field directly from there.
        let field = self.get(schema.schema())?;
        Ok(field.directives.has(inaccessible_directive)
            || self
                .parent()
                .is_inaccessible(schema, inaccessible_directive)?)
    }
}
impl IsInaccessibleExt for position::ObjectFieldArgumentDefinitionPosition {
    fn is_inaccessible(
        &self,
        schema: &FederationSchema,
        inaccessible_directive: &Name,
    ) -> Result<bool, FederationError> {
        // NOTE It'd be more efficient to start at parent and look up the field and argument directly from there.
        let argument = self.get(schema.schema())?;
        Ok(argument.directives.has(inaccessible_directive)
            || self
                .parent()
                .is_inaccessible(schema, inaccessible_directive)?)
    }
}
impl IsInaccessibleExt for position::InterfaceTypeDefinitionPosition {
    fn is_inaccessible(
        &self,
        schema: &FederationSchema,
        inaccessible_directive: &Name,
    ) -> Result<bool, FederationError> {
        let interface = self.get(schema.schema())?;
        Ok(interface.directives.has(inaccessible_directive))
    }
}
impl IsInaccessibleExt for position::InterfaceFieldDefinitionPosition {
    fn is_inaccessible(
        &self,
        schema: &FederationSchema,
        inaccessible_directive: &Name,
    ) -> Result<bool, FederationError> {
        // NOTE It'd be more efficient to start at parent and look up the field directly from there.
        let field = self.get(schema.schema())?;
        Ok(field.directives.has(inaccessible_directive)
            || self
                .parent()
                .is_inaccessible(schema, inaccessible_directive)?)
    }
}
impl IsInaccessibleExt for position::InterfaceFieldArgumentDefinitionPosition {
    fn is_inaccessible(
        &self,
        schema: &FederationSchema,
        inaccessible_directive: &Name,
    ) -> Result<bool, FederationError> {
        // NOTE It'd be more efficient to start at parent and look up the field and argument directly from there.
        let argument = self.get(schema.schema())?;
        Ok(argument.directives.has(inaccessible_directive)
            || self
                .parent()
                .is_inaccessible(schema, inaccessible_directive)?)
    }
}
impl IsInaccessibleExt for position::InputObjectTypeDefinitionPosition {
    fn is_inaccessible(
        &self,
        schema: &FederationSchema,
        inaccessible_directive: &Name,
    ) -> Result<bool, FederationError> {
        let input_object = self.get(schema.schema())?;
        Ok(input_object.directives.has(inaccessible_directive))
    }
}
impl IsInaccessibleExt for position::InputObjectFieldDefinitionPosition {
    fn is_inaccessible(
        &self,
        schema: &FederationSchema,
        inaccessible_directive: &Name,
    ) -> Result<bool, FederationError> {
        // NOTE It'd be more efficient to start at parent and look up the field directly from there.
        let field = self.get(schema.schema())?;
        Ok(field.directives.has(inaccessible_directive)
            || self
                .parent()
                .is_inaccessible(schema, inaccessible_directive)?)
    }
}
impl IsInaccessibleExt for position::ScalarTypeDefinitionPosition {
    fn is_inaccessible(
        &self,
        schema: &FederationSchema,
        inaccessible_directive: &Name,
    ) -> Result<bool, FederationError> {
        let scalar = self.get(schema.schema())?;
        Ok(scalar.directives.has(inaccessible_directive))
    }
}
impl IsInaccessibleExt for position::UnionTypeDefinitionPosition {
    fn is_inaccessible(
        &self,
        schema: &FederationSchema,
        inaccessible_directive: &Name,
    ) -> Result<bool, FederationError> {
        let union_ = self.get(schema.schema())?;
        Ok(union_.directives.has(inaccessible_directive))
    }
}
impl IsInaccessibleExt for position::EnumTypeDefinitionPosition {
    fn is_inaccessible(
        &self,
        schema: &FederationSchema,
        inaccessible_directive: &Name,
    ) -> Result<bool, FederationError> {
        let enum_ = self.get(schema.schema())?;
        Ok(enum_.directives.has(inaccessible_directive))
    }
}
impl IsInaccessibleExt for position::EnumValueDefinitionPosition {
    fn is_inaccessible(
        &self,
        schema: &FederationSchema,
        inaccessible_directive: &Name,
    ) -> Result<bool, FederationError> {
        // NOTE It'd be more efficient to start at parent and look up the value directly from there.
        let value = self.get(schema.schema())?;
        Ok(value.directives.has(inaccessible_directive)
            || self
                .parent()
                .is_inaccessible(schema, inaccessible_directive)?)
    }
}
impl IsInaccessibleExt for position::DirectiveArgumentDefinitionPosition {
    fn is_inaccessible(
        &self,
        schema: &FederationSchema,
        inaccessible_directive: &Name,
    ) -> Result<bool, FederationError> {
        let argument = self.get(schema.schema())?;
        Ok(argument.directives.has(inaccessible_directive))
    }
}

/// Types can be referenced by other schema elements in a few ways:
/// 1. Fields, arguments, and input fields may have the type as their base
///    type.
/// 2. Union types may have the type as a member (for object types).
/// 3. Object and interface types may implement the type (for interface
///    types).
/// 4. Schemas may have the type as a root operation type (for object
///    types).
///
/// When a type is hidden, the referencer must follow certain rules for the
/// schema to be valid. Respectively, these rules are:
/// 1. The field/argument/input field must not be in the API schema.
/// 2. The union type, if empty, must not be in the API schema.
/// 3. No rules are imposed in this case.
/// 4. The root operation type must not be the query type.
///
/// This function validates rules 1 and 4.
fn validate_inaccessible_type(
    schema: &FederationSchema,
    inaccessible_directive: &Name,
    position: &TypeDefinitionPosition,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let referencers = schema.referencers();

    macro_rules! check_inaccessible_reference {
        ( $ty:expr, $ref:expr ) => {
            if !$ref.is_inaccessible(schema, inaccessible_directive)? {
                errors.push(SingleFederationError::ReferencedInaccessible {
                    message: format!("Type `{}` is @inaccessible but is referenced by `{}`, which is in the API schema.", $ty, $ref),
                }.into())
            }
        }
    }

    macro_rules! check_inaccessible_referencers {
        ( $ty:expr, $( $referencers:expr ),+ ) => {
            $(
                for ref_position in $referencers {
                    check_inaccessible_reference!(position, ref_position);
                }
            )+
        }
    }

    macro_rules! missing_referencers_error {
        ( $position:expr ) => {
            SingleFederationError::Internal {
                message: format!(
                    "Type \"{}\" is marked inaccessible but does its referencers were not populated",
                    $position,
                ),
            }
        }
    }

    match position {
        TypeDefinitionPosition::Scalar(scalar_position) => {
            let referencers = referencers
                .scalar_types
                .get(&scalar_position.type_name)
                .ok_or_else(|| missing_referencers_error!(&scalar_position))?;
            check_inaccessible_referencers!(
                position,
                &referencers.object_fields,
                &referencers.interface_fields,
                &referencers.input_object_fields,
                &referencers.object_field_arguments,
                &referencers.interface_field_arguments,
                &referencers.directive_arguments
            );
        }
        TypeDefinitionPosition::Object(object_position) => {
            let referencers = referencers
                .object_types
                .get(&object_position.type_name)
                .ok_or_else(|| missing_referencers_error!(&object_position))?;
            if referencers
                .schema_roots
                .iter()
                .any(|root| root.root_kind == SchemaRootDefinitionKind::Query)
            {
                errors.push(SingleFederationError::QueryRootTypeInaccessible {
                        message: format!("Type `{position}` is @inaccessible but is the query root type, which must be in the API schema."),
                    }.into());
            }
            check_inaccessible_referencers!(
                position,
                &referencers.object_fields,
                &referencers.interface_fields
            );
        }
        TypeDefinitionPosition::Interface(interface_position) => {
            let referencers = referencers
                .interface_types
                .get(&interface_position.type_name)
                .ok_or_else(|| missing_referencers_error!(&interface_position))?;
            check_inaccessible_referencers!(
                position,
                &referencers.object_fields,
                &referencers.interface_fields
            );
        }
        TypeDefinitionPosition::Union(union_position) => {
            let referencers = referencers
                .union_types
                .get(&union_position.type_name)
                .ok_or_else(|| missing_referencers_error!(&union_position))?;
            check_inaccessible_referencers!(
                position,
                &referencers.object_fields,
                &referencers.interface_fields
            );
        }
        TypeDefinitionPosition::Enum(enum_position) => {
            let referencers = referencers
                .enum_types
                .get(&enum_position.type_name)
                .ok_or_else(|| missing_referencers_error!(&enum_position))?;
            check_inaccessible_referencers!(
                position,
                &referencers.object_fields,
                &referencers.interface_fields,
                &referencers.input_object_fields,
                &referencers.object_field_arguments,
                &referencers.interface_field_arguments,
                &referencers.directive_arguments
            );
        }
        TypeDefinitionPosition::InputObject(input_object_position) => {
            let referencers = referencers
                .input_object_types
                .get(&input_object_position.type_name)
                .ok_or_else(|| missing_referencers_error!(&input_object_position))?;
            check_inaccessible_referencers!(
                position,
                &referencers.input_object_fields,
                &referencers.object_field_arguments,
                &referencers.interface_field_arguments,
                &referencers.directive_arguments
            );
        }
    }

    Ok(())
}

fn validate_inaccessible(
    schema: &FederationSchema,
    inaccessible_spec: &InaccessibleSpecDefinition,
) -> Result<(), FederationError> {
    let inaccessible_directive = inaccessible_spec
        .directive_name_in_schema(schema, &INACCESSIBLE_DIRECTIVE_NAME_IN_SPEC)?
        .ok_or_else(|| SingleFederationError::Internal {
            message: "Unexpectedly could not find inaccessible spec in schema".to_owned(),
        })?;

    let mut errors = MultipleFederationErrors { errors: vec![] };

    // Guaranteed to exist, we would not be able to look up the `inaccessible_spec` without having
    // metadata.
    let metadata = schema.metadata().unwrap();

    for position in schema.get_types() {
        let ty = position.get(schema.schema())?;

        // Core feature directives (and their descendants) aren't allowed to be
        // @inaccessible.
        // The JavaScript implementation checks for @inaccessible on built-in types here, as well.
        // We don't do that because redefinitions of built-in types are already rejected
        // by apollo-rs validation.
        if metadata.source_link_of_type(position.type_name()).is_some() {
            if type_uses_inaccessible(schema, &inaccessible_directive, &position)? {
                errors.push(
                    SingleFederationError::DisallowedInaccessible {
                        message: format!(
                            "Core feature type `{position}` cannot use @inaccessible."
                        ),
                    }
                    .into(),
                )
            }
            continue;
        }

        let is_inaccessible = ty.directives().has(&inaccessible_directive);
        if is_inaccessible {
            validate_inaccessible_type(schema, &inaccessible_directive, &position, &mut errors)?;
        } else {
            // This type must be in the API schema. For types with children (all types except scalar),
            // we check that at least one of the children is accessible.
            match &position {
                TypeDefinitionPosition::Object(object_position) => {
                    let object = object_position.get(schema.schema())?;
                    validate_inaccessible_in_fields(
                        schema,
                        &inaccessible_directive,
                        &position,
                        &object.fields,
                        &object.implements_interfaces,
                        &mut errors,
                    )?;
                }
                TypeDefinitionPosition::Interface(interface_position) => {
                    let interface = interface_position.get(schema.schema())?;
                    validate_inaccessible_in_fields(
                        schema,
                        &inaccessible_directive,
                        &position,
                        &interface.fields,
                        &interface.implements_interfaces,
                        &mut errors,
                    )?;
                }
                TypeDefinitionPosition::InputObject(input_object_position) => {
                    let input_object = input_object_position.get(schema.schema())?;
                    let mut has_inaccessible_field = false;
                    let mut has_accessible_field = false;
                    for field in input_object.fields.values() {
                        let field_inaccessible = field.directives.has(&inaccessible_directive);
                        has_inaccessible_field |= field_inaccessible;
                        has_accessible_field |= !field_inaccessible;

                        if field_inaccessible && field.is_required() {
                            errors.push(SingleFederationError::RequiredInaccessible{
                                message: format!("Input field `{position}` is @inaccessible but is a required input field of its type."),
                            }.into());
                        }

                        if !field_inaccessible {
                            if let (Some(default_value), Some(field_type)) = (
                                &field.default_value,
                                schema.schema().types.get(field.ty.inner_named_type()),
                            ) {
                                validate_inaccessible_in_default_value(
                                    schema,
                                    &inaccessible_directive,
                                    field_type,
                                    default_value,
                                    input_object_position.field(field.name.clone()).to_string(),
                                    &mut errors,
                                )?;
                            }
                        }
                    }

                    if has_inaccessible_field && !has_accessible_field {
                        errors.push(SingleFederationError::OnlyInaccessibleChildren {
                            message: format!("Type `{position}` is in the API schema but all of its input fields are @inaccessible."),
                        }.into());
                    }
                }
                TypeDefinitionPosition::Union(union_position) => {
                    let union_ = union_position.get(schema.schema())?;
                    let types = &schema.schema().types;
                    let any_accessible_member = union_.members.iter().any(|member| {
                        !types
                            .get(&member.name)
                            .is_some_and(|ty| ty.directives().has(&inaccessible_directive))
                    });

                    if !any_accessible_member {
                        errors.push(SingleFederationError::OnlyInaccessibleChildren {
                            message: format!("Type `{position}` is in the API schema but all of its members are @inaccessible."),
                        }.into());
                    }
                }
                TypeDefinitionPosition::Enum(enum_position) => {
                    let enum_ = enum_position.get(schema.schema())?;
                    let mut has_inaccessible_value = false;
                    let mut has_accessible_value = false;
                    for value in enum_.values.values() {
                        let value_inaccessible = value.directives.has(&inaccessible_directive);
                        has_inaccessible_value |= value_inaccessible;
                        has_accessible_value |= !value_inaccessible;
                    }

                    if has_inaccessible_value && !has_accessible_value {
                        errors.push(SingleFederationError::OnlyInaccessibleChildren {
                            message: format!("Type `{enum_position}` is in the API schema but all of its members are @inaccessible."),
                        }.into());
                    }
                }
                _ => {}
            }
        }
    }

    for position in schema.get_directive_definitions() {
        let directive = position.get(schema.schema())?;
        let is_feature_directive = metadata
            .source_link_of_directive(&position.directive_name)
            .is_some();

        let mut type_system_locations = directive
            .locations
            .iter()
            .filter(|location| is_type_system_location(**location))
            .peekable();

        if is_feature_directive {
            if directive_uses_inaccessible(&inaccessible_directive, directive) {
                errors.push(
                    SingleFederationError::DisallowedInaccessible {
                        message: format!(
                            "Core feature directive `{position}` cannot use @inaccessible.",
                        ),
                    }
                    .into(),
                );
            }
        } else if type_system_locations.peek().is_some() {
            // Directives that can appear on type-system locations (and their
            // descendants) aren't allowed to be @inaccessible.
            if directive_uses_inaccessible(&inaccessible_directive, directive) {
                let type_system_locations = type_system_locations
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                errors.push(SingleFederationError::DisallowedInaccessible {
                    message: format!("Directive `{position}` cannot use @inaccessible because it may be applied to these type-system locations: {}", type_system_locations),
                }.into());
            }
        } else {
            // At this point, we know the directive must be in the API schema. Descend
            // into the directive's arguments.
            validate_inaccessible_in_arguments(
                schema,
                &inaccessible_directive,
                HasArgumentDefinitionsPosition::DirectiveDefinition(position),
                &directive.arguments,
                &mut errors,
            )?;
        }
    }

    if !errors.errors.is_empty() {
        return Err(errors.into());
    }

    Ok(())
}

fn remove_inaccessible_elements(
    schema: &mut FederationSchema,
    inaccessible_spec: &InaccessibleSpecDefinition,
) -> Result<(), FederationError> {
    let directive_name = inaccessible_spec
        .directive_name_in_schema(schema, &INACCESSIBLE_DIRECTIVE_NAME_IN_SPEC)?
        .ok_or_else(|| SingleFederationError::Internal {
            message: "Unexpectedly could not find inaccessible spec in schema".to_owned(),
        })?;

    // Find all elements that use @inaccessible. Clone so there's no live borrow.
    let inaccessible_referencers = schema.referencers().get_directive(&directive_name)?.clone();

    // Remove fields and arguments from inaccessible types first. If any inaccessible type has a field
    // that references another inaccessible type, it would prevent the other type from being
    // removed.
    // We need an intermediate allocation as `.remove()` requires mutable access to the schema and
    // looking up fields requires immutable access.
    //
    // This is a lot more verbose than in the JS implementation, but the JS implementation relies on
    // being able to set references to `undefined`, and the types all working out in the end once
    // the removal is complete, which our Rust data structures don't support.
    let mut inaccessible_children: Vec<ObjectFieldDefinitionPosition> = vec![];
    for position in &inaccessible_referencers.object_types {
        let object = position.get(schema.schema())?;
        inaccessible_children.extend(
            object
                .fields
                .keys()
                .map(|field_name| position.field(field_name.clone())),
        );
    }
    let mut inaccessible_arguments: Vec<ObjectFieldArgumentDefinitionPosition> = vec![];
    for position in inaccessible_children {
        let field = position.get(schema.schema())?;
        inaccessible_arguments.extend(
            field
                .arguments
                .iter()
                .map(|argument| position.argument(argument.name.clone())),
        );

        position.remove(schema)?;
    }
    for position in inaccessible_arguments {
        position.remove(schema)?;
    }

    let mut inaccessible_children: Vec<InterfaceFieldDefinitionPosition> = vec![];
    for position in &inaccessible_referencers.interface_types {
        let object = position.get(schema.schema())?;
        inaccessible_children.extend(
            object
                .fields
                .keys()
                .map(|field_name| position.field(field_name.clone())),
        );
    }
    let mut inaccessible_arguments: Vec<InterfaceFieldArgumentDefinitionPosition> = vec![];
    for position in inaccessible_children {
        let field = position.get(schema.schema())?;
        inaccessible_arguments.extend(
            field
                .arguments
                .iter()
                .map(|argument| position.argument(argument.name.clone())),
        );

        position.remove(schema)?;
    }
    for position in inaccessible_arguments {
        position.remove(schema)?;
    }

    let mut inaccessible_children: Vec<InputObjectFieldDefinitionPosition> = vec![];
    for position in &inaccessible_referencers.input_object_types {
        let object = position.get(schema.schema())?;
        inaccessible_children.extend(
            object
                .fields
                .keys()
                .map(|field_name| position.field(field_name.clone())),
        );
    }
    for position in inaccessible_children {
        position.remove(schema)?;
    }

    for argument in inaccessible_referencers.directive_arguments {
        argument.remove(schema)?;
    }
    for argument in inaccessible_referencers.interface_field_arguments {
        argument.remove(schema)?;
    }
    for argument in inaccessible_referencers.object_field_arguments {
        argument.remove(schema)?;
    }
    for field in inaccessible_referencers.input_object_fields {
        field.remove(schema)?;
    }
    for field in inaccessible_referencers.interface_fields {
        field.remove(schema)?;
    }
    for field in inaccessible_referencers.object_fields {
        field.remove(schema)?;
    }
    for ty in inaccessible_referencers.union_types {
        ty.remove(schema)?;
    }
    for ty in inaccessible_referencers.object_types {
        ty.remove(schema)?;
    }
    for ty in inaccessible_referencers.interface_types {
        ty.remove(schema)?;
    }
    for ty in inaccessible_referencers.input_object_types {
        ty.remove(schema)?;
    }
    for value in inaccessible_referencers.enum_values {
        value.remove(schema)?;
    }
    for ty in inaccessible_referencers.enum_types {
        ty.remove(schema)?;
    }
    for ty in inaccessible_referencers.scalar_types {
        ty.remove(schema)?;
    }

    Ok(())
}
