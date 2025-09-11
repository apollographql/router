use std::fmt::Debug;
use std::fmt::Display;
use std::fmt::Formatter;
use std::ops::Deref;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast;
use apollo_compiler::ast::Argument;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ComponentName;
use apollo_compiler::schema::ComponentOrigin;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::DirectiveDefinition;
use apollo_compiler::schema::EnumType;
use apollo_compiler::schema::EnumValueDefinition;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::FieldDefinition;
use apollo_compiler::schema::InputObjectType;
use apollo_compiler::schema::InputValueDefinition;
use apollo_compiler::schema::InterfaceType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::schema::ScalarType;
use apollo_compiler::schema::SchemaDefinition;
use apollo_compiler::schema::UnionType;
use either::Either;
use serde::Serialize;
use strum::IntoEnumIterator;

use crate::bail;
use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::link::database::links_metadata;
use crate::link::spec_definition::SpecDefinition;
use crate::merger::merge_enum::EnumExampleAst;
use crate::schema::FederationSchema;
use crate::schema::referencer::DirectiveReferencers;
use crate::schema::referencer::EnumTypeReferencers;
use crate::schema::referencer::InputObjectTypeReferencers;
use crate::schema::referencer::InterfaceTypeReferencers;
use crate::schema::referencer::ObjectTypeReferencers;
use crate::schema::referencer::Referencers;
use crate::schema::referencer::ScalarTypeReferencers;
use crate::schema::referencer::UnionTypeReferencers;

/// A zero-allocation error representation for position lookups,
/// because many of these errors are actually immediately discarded.
///
/// This type does still incur a few atomic refcount increments/decrements.
/// Maybe that could be improved in the future by borrowing from the position values,
/// if necessary.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PositionLookupError {
    #[error("Schema has no directive `{0}`")]
    DirectiveMissing(DirectiveDefinitionPosition),
    #[error("Schema has no type `{0}`")]
    TypeMissing(Name),
    #[error("Schema type `{0}` is not {1}")]
    TypeWrongKind(Name, &'static str),
    #[error("{0} type `{1}` has no field `{2}`")]
    MissingField(&'static str, Name, Name),
    #[error("Directive `{}` has no argument `{}`", .0.directive_name, .0.argument_name)]
    MissingDirectiveArgument(DirectiveArgumentDefinitionPosition),
    #[error("{0} `{1}.{2}` has no argument `{3}`")]
    MissingFieldArgument(&'static str, Name, Name, Name),
    #[error("Enum type `{}` has no value `{}`", .0.type_name, .0.value_name)]
    MissingValue(EnumValueDefinitionPosition),
    #[error("Cannot mutate reserved {0} `{1}.{2}`")]
    MutateReservedField(&'static str, Name, Name),
}

impl From<PositionLookupError> for FederationError {
    fn from(value: PositionLookupError) -> Self {
        FederationError::internal(value.to_string())
    }
}

/// The error type returned when a position conversion fails.
#[derive(Debug, thiserror::Error)]
#[error("Type `{actual}` was unexpectedly not {expected}")]
pub(crate) struct PositionConvertError<T: Debug + Display> {
    actual: T,
    expected: &'static str,
}

impl<T: Debug + Display> From<PositionConvertError<T>> for FederationError {
    fn from(value: PositionConvertError<T>) -> Self {
        FederationError::internal(value.to_string())
    }
}

/// To declare a conversion for a `Position::Branch(T) -> T`:
/// ```no_compile
/// fallible_conversions!(TypeDefinition::Scalar -> ScalarTypeDefinition);
/// ```
///
/// To declare a conversion from one enum to another, with a different set of branches:
/// ```no_compile
/// fallible_conversions!(TypeDefinition::{Scalar, Enum, InputObject} -> InputObjectTypeDefinition)
/// ```
macro_rules! fallible_conversions {
    ( $from:ident :: $branch:ident -> $to:ident ) => {
        impl TryFrom<$from> for $to {
            type Error = PositionConvertError<$from>;

            fn try_from(value: $from) -> Result<Self, Self::Error> {
                match value {
                    $from::$branch(value) => Ok(value),
                    _ => Err(PositionConvertError {
                        actual: value,
                        expected: $to::EXPECTED,
                    }),
                }
            }
        }
    };
    ( $from:ident :: { $($branch:ident),+ } -> $to:ident ) => {
        impl TryFrom<$from> for $to {
            type Error = PositionConvertError<$from>;

            fn try_from(value: $from) -> Result<Self, Self::Error> {
                match value {
                    $(
                        $from::$branch(value) => Ok($to::$branch(value)),
                    )+
                    _ => Err(PositionConvertError {
                        actual: value,
                        expected: $to::EXPECTED,
                    }),
                }
            }
        }
    }
}

/// To declare a conversion from a type to a superset type:
/// ```no_compile
/// infallible_conversions!(InputObjectTypeDefinition::{Scalar, Enum, InputObject} -> TypeDefinition)
/// ```
macro_rules! infallible_conversions {
    ( $from:ident :: { $($branch:ident),+ } -> $to:ident ) => {
        impl From<$from> for $to {
            fn from(value: $from) -> Self {
                match value {
                    $(
                        $from::$branch(value) => $to::$branch(value)
                    ),+
                }
            }
        }
    }
}

/// Makes `description` field API available for use with generic types
pub(crate) trait HasDescription {
    fn description<'schema>(&self, schema: &'schema FederationSchema)
    -> Option<&'schema Node<str>>;
    fn set_description(
        &self,
        schema: &mut FederationSchema,
        description: Option<Node<str>>,
    ) -> Result<(), FederationError>;
}

macro_rules! impl_has_description_for {
    ($struct_name:ident) => {
        impl HasDescription for $struct_name {
            fn description<'schema>(
                &self,
                schema: &'schema FederationSchema,
            ) -> Option<&'schema Node<str>> {
                self.try_get(&schema.schema)?.description.as_ref()
            }

            fn set_description(
                &self,
                schema: &mut FederationSchema,
                description: Option<Node<str>>,
            ) -> Result<(), FederationError> {
                self.make_mut(&mut schema.schema)?.make_mut().description = description;
                Ok(())
            }
        }
    };
}

impl_has_description_for!(DirectiveDefinitionPosition);
impl_has_description_for!(ScalarTypeDefinitionPosition);
impl_has_description_for!(ObjectTypeDefinitionPosition);
impl_has_description_for!(InterfaceTypeDefinitionPosition);
impl_has_description_for!(UnionTypeDefinitionPosition);
impl_has_description_for!(EnumTypeDefinitionPosition);
impl_has_description_for!(InputObjectTypeDefinitionPosition);
impl_has_description_for!(ObjectFieldDefinitionPosition);
impl_has_description_for!(InterfaceFieldDefinitionPosition);
impl_has_description_for!(EnumValueDefinitionPosition);
impl_has_description_for!(InputObjectFieldDefinitionPosition);

// Irregular implementations of HasDescription
impl HasDescription for SchemaDefinitionPosition {
    fn description<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Option<&'schema Node<str>> {
        self.get(&schema.schema).description.as_ref()
    }

    fn set_description(
        &self,
        schema: &mut FederationSchema,
        description: Option<Node<str>>,
    ) -> Result<(), FederationError> {
        self.make_mut(&mut schema.schema).make_mut().description = description;
        Ok(())
    }
}

impl HasDescription for ObjectOrInterfaceFieldDefinitionPosition {
    fn description<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Option<&'schema Node<str>> {
        match self {
            Self::Object(field) => field.description(schema),
            Self::Interface(field) => field.description(schema),
        }
    }

    fn set_description(
        &self,
        schema: &mut FederationSchema,
        description: Option<Node<str>>,
    ) -> Result<(), FederationError> {
        match self {
            Self::Object(field) => field.set_description(schema, description),
            Self::Interface(field) => field.set_description(schema, description),
        }
    }
}

impl HasDescription for FieldDefinitionPosition {
    fn description<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Option<&'schema Node<str>> {
        match self {
            FieldDefinitionPosition::Object(field) => field.description(schema),
            FieldDefinitionPosition::Interface(field) => field.description(schema),
            FieldDefinitionPosition::Union(field) => field.description(schema),
        }
    }

    fn set_description(
        &self,
        schema: &mut FederationSchema,
        description: Option<Node<str>>,
    ) -> Result<(), FederationError> {
        match self {
            FieldDefinitionPosition::Object(field) => field.set_description(schema, description),
            FieldDefinitionPosition::Interface(field) => field.set_description(schema, description),
            FieldDefinitionPosition::Union(field) => field.set_description(schema, description),
        }
    }
}

impl HasDescription for UnionTypenameFieldDefinitionPosition {
    fn description<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Option<&'schema Node<str>> {
        self.get(&schema.schema)
            .map_or(None, |field| field.description.as_ref())
    }

    fn set_description(
        &self,
        _schema: &mut FederationSchema,
        _description: Option<Node<str>>,
    ) -> Result<(), FederationError> {
        bail!("Description is immutable for union typename fields")
    }
}

impl HasDescription for DirectiveTargetPosition {
    fn description<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Option<&'schema Node<str>> {
        match self {
            Self::Schema(pos) => pos.description(schema),
            Self::ScalarType(pos) => pos.description(schema),
            Self::ObjectType(pos) => pos.description(schema),
            Self::ObjectField(pos) => pos.description(schema),
            Self::InterfaceType(pos) => pos.description(schema),
            Self::InterfaceField(pos) => pos.description(schema),
            Self::UnionType(pos) => pos.description(schema),
            Self::EnumType(pos) => pos.description(schema),
            Self::EnumValue(pos) => pos.description(schema),
            Self::InputObjectType(pos) => pos.description(schema),
            _ => None,
        }
    }

    fn set_description(
        &self,
        schema: &mut FederationSchema,
        description: Option<Node<str>>,
    ) -> Result<(), FederationError> {
        match self {
            Self::Schema(pos) => pos.set_description(schema, description),
            Self::ScalarType(pos) => pos.set_description(schema, description),
            Self::ObjectType(pos) => pos.set_description(schema, description),
            Self::ObjectField(pos) => pos.set_description(schema, description),
            Self::InterfaceType(pos) => pos.set_description(schema, description),
            Self::InterfaceField(pos) => pos.set_description(schema, description),
            Self::UnionType(pos) => pos.set_description(schema, description),
            Self::EnumType(pos) => pos.set_description(schema, description),
            Self::EnumValue(pos) => pos.set_description(schema, description),
            Self::InputObjectType(pos) => pos.set_description(schema, description),
            _ => Err(FederationError::SingleFederationError(
                SingleFederationError::Internal {
                    message: String::from(
                        "No valid conversion from DirectiveTargetPosition to desired type.",
                    ),
                },
            )),
        }
    }
}

pub(crate) trait HasType {
    fn get_type<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema ast::Type, FederationError>;

    fn set_type(&self, schema: &mut FederationSchema, ty: ast::Type)
    -> Result<(), FederationError>;

    fn enum_example_ast(
        &self,
        schema: &FederationSchema,
    ) -> Result<EnumExampleAst, FederationError>;
}

impl HasType for FieldArgumentDefinitionPosition {
    fn get_type<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema ast::Type, FederationError> {
        Ok(self.get(&schema.schema)?.ty.as_ref())
    }

    fn set_type(
        &self,
        schema: &mut FederationSchema,
        ty: ast::Type,
    ) -> Result<(), FederationError> {
        *self.make_mut(&mut schema.schema)?.make_mut().ty.make_mut() = ty;
        Ok(())
    }

    fn enum_example_ast(
        &self,
        schema: &FederationSchema,
    ) -> Result<EnumExampleAst, FederationError> {
        let node = self.get(schema.schema())?.clone();
        Ok(EnumExampleAst::Input(node))
    }
}

impl HasType for InputObjectFieldDefinitionPosition {
    fn get_type<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema ast::Type, FederationError> {
        Ok(&self.get(&schema.schema)?.ty)
    }

    fn set_type(
        &self,
        schema: &mut FederationSchema,
        ty: ast::Type,
    ) -> Result<(), FederationError> {
        *self.make_mut(&mut schema.schema)?.make_mut().ty.make_mut() = ty;
        Ok(())
    }

    fn enum_example_ast(
        &self,
        schema: &FederationSchema,
    ) -> Result<EnumExampleAst, FederationError> {
        Ok(EnumExampleAst::Input(
            self.get(schema.schema())?.clone().node,
        ))
    }
}

impl HasType for ObjectFieldDefinitionPosition {
    fn get_type<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema ast::Type, FederationError> {
        Ok(&self.get(&schema.schema)?.ty)
    }

    fn set_type(
        &self,
        schema: &mut FederationSchema,
        ty: ast::Type,
    ) -> Result<(), FederationError> {
        self.make_mut(&mut schema.schema)?.make_mut().ty = ty;
        Ok(())
    }

    fn enum_example_ast(
        &self,
        schema: &FederationSchema,
    ) -> Result<EnumExampleAst, FederationError> {
        let node = self.get(schema.schema())?.clone().node;
        Ok(EnumExampleAst::Field(node))
    }
}

impl HasType for InterfaceFieldDefinitionPosition {
    fn get_type<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema ast::Type, FederationError> {
        Ok(&self.get(&schema.schema)?.ty)
    }

    fn set_type(
        &self,
        schema: &mut FederationSchema,
        ty: ast::Type,
    ) -> Result<(), FederationError> {
        self.make_mut(&mut schema.schema)?.make_mut().ty = ty;
        Ok(())
    }

    fn enum_example_ast(
        &self,
        schema: &FederationSchema,
    ) -> Result<EnumExampleAst, FederationError> {
        let node = self.get(schema.schema())?.clone().node;
        Ok(EnumExampleAst::Field(node))
    }
}

impl HasType for ObjectOrInterfaceFieldDefinitionPosition {
    fn get_type<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema ast::Type, FederationError> {
        match self {
            Self::Object(field) => field.get_type(schema),
            Self::Interface(field) => field.get_type(schema),
        }
    }

    fn set_type(
        &self,
        schema: &mut FederationSchema,
        ty: ast::Type,
    ) -> Result<(), FederationError> {
        match self {
            Self::Object(field) => field.set_type(schema, ty),
            Self::Interface(field) => field.set_type(schema, ty),
        }
    }

    fn enum_example_ast(
        &self,
        schema: &FederationSchema,
    ) -> Result<EnumExampleAst, FederationError> {
        match self {
            Self::Object(field) => field.enum_example_ast(schema),
            Self::Interface(field) => field.enum_example_ast(schema),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, derive_more::From, derive_more::Display)]
pub(crate) enum TypeDefinitionPosition {
    Scalar(ScalarTypeDefinitionPosition),
    Object(ObjectTypeDefinitionPosition),
    Interface(InterfaceTypeDefinitionPosition),
    Union(UnionTypeDefinitionPosition),
    Enum(EnumTypeDefinitionPosition),
    InputObject(InputObjectTypeDefinitionPosition),
}

impl Debug for TypeDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Scalar(p) => write!(f, "Scalar({p})"),
            Self::Object(p) => write!(f, "Object({p})"),
            Self::Interface(p) => write!(f, "Interface({p})"),
            Self::Union(p) => write!(f, "Union({p})"),
            Self::Enum(p) => write!(f, "Enum({p})"),
            Self::InputObject(p) => write!(f, "InputObject({p})"),
        }
    }
}

impl TypeDefinitionPosition {
    pub(crate) fn is_composite_type(&self) -> bool {
        matches!(
            self,
            TypeDefinitionPosition::Object(_)
                | TypeDefinitionPosition::Interface(_)
                | TypeDefinitionPosition::Union(_)
        )
    }

    pub(crate) fn is_introspection_type(&self) -> bool {
        self.type_name().starts_with("__")
    }

    pub(crate) fn is_object_type(&self) -> bool {
        matches!(self, TypeDefinitionPosition::Object(_))
    }

    pub(crate) fn type_name(&self) -> &Name {
        match self {
            TypeDefinitionPosition::Scalar(type_) => &type_.type_name,
            TypeDefinitionPosition::Object(type_) => &type_.type_name,
            TypeDefinitionPosition::Interface(type_) => &type_.type_name,
            TypeDefinitionPosition::Union(type_) => &type_.type_name,
            TypeDefinitionPosition::Enum(type_) => &type_.type_name,
            TypeDefinitionPosition::InputObject(type_) => &type_.type_name,
        }
    }

    pub(crate) fn kind(&self) -> &'static str {
        match self {
            TypeDefinitionPosition::Object(_) => "ObjectType",
            TypeDefinitionPosition::Interface(_) => "InterfaceType",
            TypeDefinitionPosition::Union(_) => "UnionType",
            TypeDefinitionPosition::Enum(_) => "EnumType",
            TypeDefinitionPosition::Scalar(_) => "ScalarType",
            TypeDefinitionPosition::InputObject(_) => "InputObjectType",
        }
    }

    fn describe(&self) -> &'static str {
        match self {
            TypeDefinitionPosition::Scalar(_) => ScalarTypeDefinitionPosition::EXPECTED,
            TypeDefinitionPosition::Object(_) => ObjectTypeDefinitionPosition::EXPECTED,
            TypeDefinitionPosition::Interface(_) => InterfaceTypeDefinitionPosition::EXPECTED,
            TypeDefinitionPosition::Union(_) => UnionTypeDefinitionPosition::EXPECTED,
            TypeDefinitionPosition::Enum(_) => EnumTypeDefinitionPosition::EXPECTED,
            TypeDefinitionPosition::InputObject(_) => InputObjectTypeDefinitionPosition::EXPECTED,
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema ExtendedType, PositionLookupError> {
        let name = self.type_name();
        let ty = schema
            .types
            .get(name)
            .ok_or_else(|| PositionLookupError::TypeMissing(name.clone()))?;
        match (ty, self) {
            (ExtendedType::Scalar(_), TypeDefinitionPosition::Scalar(_))
            | (ExtendedType::Object(_), TypeDefinitionPosition::Object(_))
            | (ExtendedType::Interface(_), TypeDefinitionPosition::Interface(_))
            | (ExtendedType::Union(_), TypeDefinitionPosition::Union(_))
            | (ExtendedType::Enum(_), TypeDefinitionPosition::Enum(_))
            | (ExtendedType::InputObject(_), TypeDefinitionPosition::InputObject(_)) => Ok(ty),
            _ => Err(PositionLookupError::TypeWrongKind(
                name.clone(),
                self.describe(),
            )),
        }
    }

    pub(crate) fn insert_directive(
        &self,
        schema: &mut FederationSchema,
        directive: Component<Directive>,
    ) -> Result<(), FederationError> {
        match self {
            TypeDefinitionPosition::Scalar(type_) => type_.insert_directive(schema, directive),
            TypeDefinitionPosition::Object(type_) => type_.insert_directive(schema, directive),
            TypeDefinitionPosition::Interface(type_) => type_.insert_directive(schema, directive),
            TypeDefinitionPosition::Union(type_) => type_.insert_directive(schema, directive),
            TypeDefinitionPosition::Enum(type_) => type_.insert_directive(schema, directive),
            TypeDefinitionPosition::InputObject(type_) => type_.insert_directive(schema, directive),
        }
    }

    pub(crate) fn rename(
        &self,
        schema: &mut FederationSchema,
        new_name: Name,
    ) -> Result<(), FederationError> {
        match self {
            TypeDefinitionPosition::Scalar(type_) => type_.rename(schema, new_name.clone())?,
            TypeDefinitionPosition::Object(type_) => type_.rename(schema, new_name.clone())?,
            TypeDefinitionPosition::Interface(type_) => type_.rename(schema, new_name.clone())?,
            TypeDefinitionPosition::Union(type_) => type_.rename(schema, new_name.clone())?,
            TypeDefinitionPosition::Enum(type_) => type_.rename(schema, new_name.clone())?,
            TypeDefinitionPosition::InputObject(type_) => type_.rename(schema, new_name.clone())?,
        }

        if let Some(existing_type) = schema.schema.types.swap_remove(self.type_name()) {
            schema.schema.types.insert(new_name, existing_type);
        }

        Ok(())
    }

    pub(crate) fn remove_extensions(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        match self {
            TypeDefinitionPosition::Scalar(type_) => type_.remove_extensions(schema),
            TypeDefinitionPosition::Object(type_) => type_.remove_extensions(schema),
            TypeDefinitionPosition::Interface(type_) => type_.remove_extensions(schema),
            TypeDefinitionPosition::Union(type_) => type_.remove_extensions(schema),
            TypeDefinitionPosition::Enum(type_) => type_.remove_extensions(schema),
            TypeDefinitionPosition::InputObject(type_) => type_.remove_extensions(schema),
        }
    }

    pub(crate) fn has_applied_directive(
        &self,
        schema: &FederationSchema,
        directive_name: &Name,
    ) -> bool {
        match self {
            TypeDefinitionPosition::Scalar(type_) => {
                type_.has_applied_directive(schema, directive_name)
            }
            TypeDefinitionPosition::Object(type_) => {
                type_.has_applied_directive(schema, directive_name)
            }
            TypeDefinitionPosition::Interface(type_) => {
                type_.has_applied_directive(schema, directive_name)
            }
            TypeDefinitionPosition::Union(type_) => {
                type_.has_applied_directive(schema, directive_name)
            }
            TypeDefinitionPosition::Enum(type_) => {
                type_.has_applied_directive(schema, directive_name)
            }
            TypeDefinitionPosition::InputObject(type_) => {
                type_.has_applied_directive(schema, directive_name)
            }
        }
    }

    pub(crate) fn get_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
        directive_name: &Name,
    ) -> Vec<&'schema Component<Directive>> {
        match self {
            TypeDefinitionPosition::Scalar(type_) => {
                type_.get_applied_directives(schema, directive_name)
            }
            TypeDefinitionPosition::Object(type_) => {
                type_.get_applied_directives(schema, directive_name)
            }
            TypeDefinitionPosition::Interface(type_) => {
                type_.get_applied_directives(schema, directive_name)
            }
            TypeDefinitionPosition::Union(type_) => {
                type_.get_applied_directives(schema, directive_name)
            }
            TypeDefinitionPosition::Enum(type_) => {
                type_.get_applied_directives(schema, directive_name)
            }
            TypeDefinitionPosition::InputObject(type_) => {
                type_.get_applied_directives(schema, directive_name)
            }
        }
    }

    /// Remove a directive application.
    #[allow(unused)]
    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Component<Directive>,
    ) {
        match self {
            TypeDefinitionPosition::Scalar(type_) => type_.remove_directive(schema, directive),
            TypeDefinitionPosition::Object(type_) => type_.remove_directive(schema, directive),
            TypeDefinitionPosition::Interface(type_) => type_.remove_directive(schema, directive),
            TypeDefinitionPosition::Union(type_) => type_.remove_directive(schema, directive),
            TypeDefinitionPosition::Enum(type_) => type_.remove_directive(schema, directive),
            TypeDefinitionPosition::InputObject(type_) => type_.remove_directive(schema, directive),
        }
    }

    pub(crate) fn pre_insert(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        match self {
            TypeDefinitionPosition::Scalar(type_) => type_.pre_insert(schema),
            TypeDefinitionPosition::Object(type_) => type_.pre_insert(schema),
            TypeDefinitionPosition::Interface(type_) => type_.pre_insert(schema),
            TypeDefinitionPosition::Union(type_) => type_.pre_insert(schema),
            TypeDefinitionPosition::Enum(type_) => type_.pre_insert(schema),
            TypeDefinitionPosition::InputObject(type_) => type_.pre_insert(schema),
        }
    }

    /// Inserts a new empty type with this position's type name into the schema.
    /// This is used during passes where we shallow-copy types from schema to schema.
    pub(crate) fn insert_empty(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        match self {
            TypeDefinitionPosition::Scalar(type_) => type_.insert(
                schema,
                Node::new(ScalarType {
                    description: None,
                    name: self.type_name().clone(),
                    directives: Default::default(),
                }),
            ),
            TypeDefinitionPosition::Object(type_) => type_.insert(
                schema,
                Node::new(ObjectType {
                    description: None,
                    name: self.type_name().clone(),
                    implements_interfaces: Default::default(),
                    fields: Default::default(),
                    directives: Default::default(),
                }),
            ),
            TypeDefinitionPosition::Interface(type_) => type_.insert_empty(schema),
            TypeDefinitionPosition::Union(type_) => type_.insert(
                schema,
                Node::new(UnionType {
                    description: None,
                    name: self.type_name().clone(),
                    members: Default::default(),
                    directives: Default::default(),
                }),
            ),
            TypeDefinitionPosition::Enum(type_) => type_.insert(
                schema,
                Node::new(EnumType {
                    description: None,
                    name: self.type_name().clone(),
                    values: Default::default(),
                    directives: Default::default(),
                }),
            ),
            TypeDefinitionPosition::InputObject(type_) => type_.insert(
                schema,
                Node::new(InputObjectType {
                    description: None,
                    name: self.type_name().clone(),
                    fields: Default::default(),
                    directives: Default::default(),
                }),
            ),
        }
    }

    pub(crate) fn remove(&self, schema: &mut FederationSchema) -> Result<bool, FederationError> {
        let is_some = match self {
            TypeDefinitionPosition::Scalar(scalar_pos) => scalar_pos.remove(schema)?.is_some(),
            TypeDefinitionPosition::Enum(enum_pos) => enum_pos.remove(schema)?.is_some(),
            TypeDefinitionPosition::Object(object_pos) => object_pos.remove(schema)?.is_some(),
            TypeDefinitionPosition::Interface(interface_pos) => {
                interface_pos.remove(schema)?.is_some()
            }
            TypeDefinitionPosition::Union(union_pos) => union_pos.remove(schema)?.is_some(),
            TypeDefinitionPosition::InputObject(input_object_pos) => {
                input_object_pos.remove(schema)?.is_some()
            }
        };
        Ok(is_some)
    }

    #[allow(unused)]
    pub(crate) fn remove_recursive(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        match self {
            TypeDefinitionPosition::Scalar(scalar_pos) => {
                // Note: No `remove_recursive` for scalars
                _ = scalar_pos.remove(schema)?;
            }
            TypeDefinitionPosition::Enum(enum_pos) => {
                // Note: No `remove_recursive` for enums
                _ = enum_pos.remove(schema)?;
            }
            TypeDefinitionPosition::Object(object_pos) => {
                object_pos.remove_recursive(schema)?;
            }
            TypeDefinitionPosition::Interface(interface_pos) => {
                interface_pos.remove_recursive(schema)?;
            }
            TypeDefinitionPosition::Union(union_pos) => {
                union_pos.remove_recursive(schema)?;
            }
            TypeDefinitionPosition::InputObject(input_object_pos) => {
                input_object_pos.remove_recursive(schema)?;
            }
        };
        Ok(())
    }
}

impl From<&ExtendedType> for TypeDefinitionPosition {
    fn from(ty: &ExtendedType) -> Self {
        match ty {
            ExtendedType::Scalar(v) => {
                TypeDefinitionPosition::Scalar(ScalarTypeDefinitionPosition {
                    type_name: v.name.clone(),
                })
            }
            ExtendedType::Object(v) => {
                TypeDefinitionPosition::Object(ObjectTypeDefinitionPosition {
                    type_name: v.name.clone(),
                })
            }
            ExtendedType::Interface(v) => {
                TypeDefinitionPosition::Interface(InterfaceTypeDefinitionPosition {
                    type_name: v.name.clone(),
                })
            }
            ExtendedType::Union(v) => TypeDefinitionPosition::Union(UnionTypeDefinitionPosition {
                type_name: v.name.clone(),
            }),
            ExtendedType::Enum(v) => TypeDefinitionPosition::Enum(EnumTypeDefinitionPosition {
                type_name: v.name.clone(),
            }),
            ExtendedType::InputObject(v) => {
                TypeDefinitionPosition::InputObject(InputObjectTypeDefinitionPosition {
                    type_name: v.name.clone(),
                })
            }
        }
    }
}

impl HasDescription for TypeDefinitionPosition {
    fn description<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Option<&'schema Node<str>> {
        match self {
            Self::Scalar(ty) => ty.description(schema),
            Self::Object(ty) => ty.description(schema),
            Self::Interface(ty) => ty.description(schema),
            Self::Union(ty) => ty.description(schema),
            Self::Enum(ty) => ty.description(schema),
            Self::InputObject(ty) => ty.description(schema),
        }
    }

    fn set_description(
        &self,
        schema: &mut FederationSchema,
        description: Option<Node<str>>,
    ) -> Result<(), FederationError> {
        match self {
            Self::Scalar(ty) => ty.set_description(schema, description),
            Self::Object(ty) => ty.set_description(schema, description),
            Self::Interface(ty) => ty.set_description(schema, description),
            Self::Union(ty) => ty.set_description(schema, description),
            Self::Enum(ty) => ty.set_description(schema, description),
            Self::InputObject(ty) => ty.set_description(schema, description),
        }
    }
}

fallible_conversions!(TypeDefinitionPosition::Scalar -> ScalarTypeDefinitionPosition);
fallible_conversions!(TypeDefinitionPosition::Object -> ObjectTypeDefinitionPosition);
fallible_conversions!(TypeDefinitionPosition::Interface -> InterfaceTypeDefinitionPosition);
fallible_conversions!(TypeDefinitionPosition::Union -> UnionTypeDefinitionPosition);
fallible_conversions!(TypeDefinitionPosition::Enum -> EnumTypeDefinitionPosition);
fallible_conversions!(TypeDefinitionPosition::InputObject -> InputObjectTypeDefinitionPosition);

infallible_conversions!(OutputTypeDefinitionPosition::{Scalar, Object, Interface, Union, Enum} -> TypeDefinitionPosition);
infallible_conversions!(CompositeTypeDefinitionPosition::{Object, Interface, Union} -> TypeDefinitionPosition);
infallible_conversions!(AbstractTypeDefinitionPosition::{Interface, Union} -> TypeDefinitionPosition);
infallible_conversions!(ObjectOrInterfaceTypeDefinitionPosition::{Object, Interface} -> TypeDefinitionPosition);

#[derive(Clone, PartialEq, Eq, Hash, derive_more::From, derive_more::Display)]
pub(crate) enum OutputTypeDefinitionPosition {
    Scalar(ScalarTypeDefinitionPosition),
    Object(ObjectTypeDefinitionPosition),
    Interface(InterfaceTypeDefinitionPosition),
    Union(UnionTypeDefinitionPosition),
    Enum(EnumTypeDefinitionPosition),
}

impl Debug for OutputTypeDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Scalar(p) => write!(f, "Scalar({p})"),
            Self::Object(p) => write!(f, "Object({p})"),
            Self::Interface(p) => write!(f, "Interface({p})"),
            Self::Union(p) => write!(f, "Union({p})"),
            Self::Enum(p) => write!(f, "Enum({p})"),
        }
    }
}

impl OutputTypeDefinitionPosition {
    const EXPECTED: &'static str = "an output type";

    pub(crate) fn is_leaf_type(&self) -> bool {
        matches!(
            self,
            OutputTypeDefinitionPosition::Scalar(_) | OutputTypeDefinitionPosition::Enum(_)
        )
    }

    pub(crate) fn type_name(&self) -> &Name {
        match self {
            OutputTypeDefinitionPosition::Scalar(type_) => &type_.type_name,
            OutputTypeDefinitionPosition::Object(type_) => &type_.type_name,
            OutputTypeDefinitionPosition::Interface(type_) => &type_.type_name,
            OutputTypeDefinitionPosition::Union(type_) => &type_.type_name,
            OutputTypeDefinitionPosition::Enum(type_) => &type_.type_name,
        }
    }
}

fallible_conversions!(OutputTypeDefinitionPosition::Scalar -> ScalarTypeDefinitionPosition);
fallible_conversions!(OutputTypeDefinitionPosition::Object -> ObjectTypeDefinitionPosition);
fallible_conversions!(OutputTypeDefinitionPosition::Interface -> InterfaceTypeDefinitionPosition);
fallible_conversions!(OutputTypeDefinitionPosition::Union -> UnionTypeDefinitionPosition);
fallible_conversions!(OutputTypeDefinitionPosition::Enum -> EnumTypeDefinitionPosition);
fallible_conversions!(TypeDefinitionPosition::{Scalar, Object, Interface, Enum, Union} -> OutputTypeDefinitionPosition);

infallible_conversions!(CompositeTypeDefinitionPosition::{Object, Interface, Union} -> OutputTypeDefinitionPosition);
infallible_conversions!(AbstractTypeDefinitionPosition::{Interface, Union} -> OutputTypeDefinitionPosition);
infallible_conversions!(ObjectOrInterfaceTypeDefinitionPosition::{Object, Interface} -> OutputTypeDefinitionPosition);

#[derive(Clone, PartialEq, Eq, Hash, derive_more::From, derive_more::Display, Serialize)]
pub(crate) enum CompositeTypeDefinitionPosition {
    Object(ObjectTypeDefinitionPosition),
    Interface(InterfaceTypeDefinitionPosition),
    Union(UnionTypeDefinitionPosition),
}

impl Debug for CompositeTypeDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Object(p) => write!(f, "Object({p})"),
            Self::Interface(p) => write!(f, "Interface({p})"),
            Self::Union(p) => write!(f, "Union({p})"),
        }
    }
}

impl CompositeTypeDefinitionPosition {
    const EXPECTED: &'static str = "a composite type";

    pub(crate) fn is_object_type(&self) -> bool {
        matches!(self, CompositeTypeDefinitionPosition::Object(_))
    }

    pub(crate) fn is_interface_type(&self) -> bool {
        matches!(self, CompositeTypeDefinitionPosition::Interface(_))
    }

    pub(crate) fn is_union_type(&self) -> bool {
        matches!(self, CompositeTypeDefinitionPosition::Union(_))
    }

    pub(crate) fn is_abstract_type(&self) -> bool {
        self.is_interface_type() || self.is_union_type()
    }

    pub(crate) fn type_name(&self) -> &Name {
        match self {
            CompositeTypeDefinitionPosition::Object(type_) => &type_.type_name,
            CompositeTypeDefinitionPosition::Interface(type_) => &type_.type_name,
            CompositeTypeDefinitionPosition::Union(type_) => &type_.type_name,
        }
    }

    fn describe(&self) -> &'static str {
        match self {
            CompositeTypeDefinitionPosition::Object(_) => ObjectTypeDefinitionPosition::EXPECTED,
            CompositeTypeDefinitionPosition::Interface(_) => {
                InterfaceTypeDefinitionPosition::EXPECTED
            }
            CompositeTypeDefinitionPosition::Union(_) => UnionTypeDefinitionPosition::EXPECTED,
        }
    }

    pub(crate) fn field(
        &self,
        field_name: Name,
    ) -> Result<FieldDefinitionPosition, FederationError> {
        match self {
            CompositeTypeDefinitionPosition::Object(type_) => Ok(type_.field(field_name).into()),
            CompositeTypeDefinitionPosition::Interface(type_) => Ok(type_.field(field_name).into()),
            CompositeTypeDefinitionPosition::Union(type_) => {
                let field = type_.introspection_typename_field();
                if *field.field_name() == field_name {
                    Ok(field.into())
                } else {
                    Err(FederationError::internal(format!(
                        r#"Union types don't have field "{}", only "{}""#,
                        field_name,
                        field.field_name(),
                    )))
                }
            }
        }
    }

    pub(crate) fn introspection_typename_field(&self) -> FieldDefinitionPosition {
        match self {
            CompositeTypeDefinitionPosition::Object(type_) => {
                type_.introspection_typename_field().into()
            }
            CompositeTypeDefinitionPosition::Interface(type_) => {
                type_.introspection_typename_field().into()
            }
            CompositeTypeDefinitionPosition::Union(type_) => {
                type_.introspection_typename_field().into()
            }
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema ExtendedType, PositionLookupError> {
        let name = self.type_name();
        let ty = schema
            .types
            .get(name)
            .ok_or_else(|| PositionLookupError::TypeMissing(name.clone()))?;
        match (ty, self) {
            (ExtendedType::Object(_), CompositeTypeDefinitionPosition::Object(_))
            | (ExtendedType::Interface(_), CompositeTypeDefinitionPosition::Interface(_))
            | (ExtendedType::Union(_), CompositeTypeDefinitionPosition::Union(_)) => Ok(ty),
            _ => Err(PositionLookupError::TypeWrongKind(
                name.clone(),
                self.describe(),
            )),
        }
    }

    pub(crate) fn get_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
        directive_name: &Name,
    ) -> Vec<&'schema Component<Directive>> {
        match self {
            CompositeTypeDefinitionPosition::Object(type_) => {
                type_.get_applied_directives(schema, directive_name)
            }
            CompositeTypeDefinitionPosition::Interface(type_) => {
                type_.get_applied_directives(schema, directive_name)
            }
            CompositeTypeDefinitionPosition::Union(type_) => {
                type_.get_applied_directives(schema, directive_name)
            }
        }
    }

    pub(crate) fn insert_directive(
        &self,
        schema: &mut FederationSchema,
        directive: Component<Directive>,
    ) -> Result<(), FederationError> {
        match self {
            CompositeTypeDefinitionPosition::Object(type_) => {
                type_.insert_directive(schema, directive)
            }
            CompositeTypeDefinitionPosition::Interface(type_) => {
                type_.insert_directive(schema, directive)
            }
            CompositeTypeDefinitionPosition::Union(type_) => {
                type_.insert_directive(schema, directive)
            }
        }
    }
}

fallible_conversions!(CompositeTypeDefinitionPosition::Object -> ObjectTypeDefinitionPosition);
fallible_conversions!(CompositeTypeDefinitionPosition::Interface -> InterfaceTypeDefinitionPosition);
fallible_conversions!(CompositeTypeDefinitionPosition::Union -> UnionTypeDefinitionPosition);

fallible_conversions!(TypeDefinitionPosition::{Object, Interface, Union} -> CompositeTypeDefinitionPosition);
fallible_conversions!(OutputTypeDefinitionPosition::{Object, Interface, Union} -> CompositeTypeDefinitionPosition);
infallible_conversions!(AbstractTypeDefinitionPosition::{Interface, Union} -> CompositeTypeDefinitionPosition);
infallible_conversions!(ObjectOrInterfaceTypeDefinitionPosition::{Object, Interface} -> CompositeTypeDefinitionPosition);

#[derive(Clone, PartialEq, Eq, Hash, derive_more::From, derive_more::Display)]
pub(crate) enum AbstractTypeDefinitionPosition {
    Interface(InterfaceTypeDefinitionPosition),
    Union(UnionTypeDefinitionPosition),
}

impl Debug for AbstractTypeDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Interface(p) => write!(f, "Interface({p})"),
            Self::Union(p) => write!(f, "Union({p})"),
        }
    }
}

impl AbstractTypeDefinitionPosition {
    const EXPECTED: &'static str = "an abstract type";

    pub(crate) fn type_name(&self) -> &Name {
        match self {
            AbstractTypeDefinitionPosition::Interface(type_) => &type_.type_name,
            AbstractTypeDefinitionPosition::Union(type_) => &type_.type_name,
        }
    }
}

fallible_conversions!(AbstractTypeDefinitionPosition::Interface -> InterfaceTypeDefinitionPosition);
fallible_conversions!(AbstractTypeDefinitionPosition::Union -> UnionTypeDefinitionPosition);
fallible_conversions!(TypeDefinitionPosition::{Interface, Union} -> AbstractTypeDefinitionPosition);
fallible_conversions!(OutputTypeDefinitionPosition::{Interface, Union} -> AbstractTypeDefinitionPosition);
fallible_conversions!(CompositeTypeDefinitionPosition::{Interface, Union} -> AbstractTypeDefinitionPosition);
fallible_conversions!(ObjectOrInterfaceTypeDefinitionPosition::{Interface} -> AbstractTypeDefinitionPosition);

#[derive(Clone, PartialEq, Eq, Hash, derive_more::From, derive_more::Display)]
pub(crate) enum ObjectOrInterfaceTypeDefinitionPosition {
    Object(ObjectTypeDefinitionPosition),
    Interface(InterfaceTypeDefinitionPosition),
}

impl Debug for ObjectOrInterfaceTypeDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Object(p) => write!(f, "Object({p})"),
            Self::Interface(p) => write!(f, "Interface({p})"),
        }
    }
}

impl ObjectOrInterfaceTypeDefinitionPosition {
    const EXPECTED: &'static str = "an object/interface type";

    pub(crate) fn type_name(&self) -> &Name {
        match self {
            ObjectOrInterfaceTypeDefinitionPosition::Object(type_) => &type_.type_name,
            ObjectOrInterfaceTypeDefinitionPosition::Interface(type_) => &type_.type_name,
        }
    }

    pub(crate) fn field(&self, field_name: Name) -> ObjectOrInterfaceFieldDefinitionPosition {
        match self {
            ObjectOrInterfaceTypeDefinitionPosition::Object(type_) => {
                type_.field(field_name).into()
            }
            ObjectOrInterfaceTypeDefinitionPosition::Interface(type_) => {
                type_.field(field_name).into()
            }
        }
    }

    pub(crate) fn fields<'a>(
        &'a self,
        schema: &'a Schema,
    ) -> Result<impl Iterator<Item = ObjectOrInterfaceFieldDefinitionPosition>, FederationError>
    {
        match self {
            ObjectOrInterfaceTypeDefinitionPosition::Object(type_) => Ok(Either::Left(
                type_.fields(schema)?.map(|field| field.into()),
            )),
            ObjectOrInterfaceTypeDefinitionPosition::Interface(type_) => Ok(Either::Right(
                type_.fields(schema)?.map(|field| field.into()),
            )),
        }
    }

    pub(crate) fn insert_directive(
        &self,
        schema: &mut FederationSchema,
        directive: Component<Directive>,
    ) -> Result<(), FederationError> {
        match self {
            Self::Object(type_) => type_.insert_directive(schema, directive),
            Self::Interface(type_) => type_.insert_directive(schema, directive),
        }
    }

    pub(crate) fn implemented_interfaces<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<&'schema apollo_compiler::collections::IndexSet<ComponentName>, PositionLookupError>
    {
        match self {
            Self::Object(type_) => type_
                .get(schema.schema())
                .map(|obj| &obj.implements_interfaces),
            Self::Interface(type_) => type_
                .get(schema.schema())
                .map(|itf| &itf.implements_interfaces),
        }
    }

    pub(crate) fn insert_implements_interface(
        &self,
        schema: &mut FederationSchema,
        interface_name: ComponentName,
    ) -> Result<(), FederationError> {
        match self {
            Self::Object(type_) => type_.insert_implements_interface(schema, interface_name),
            Self::Interface(type_) => type_.insert_implements_interface(schema, interface_name),
        }
    }
}

fallible_conversions!(ObjectOrInterfaceTypeDefinitionPosition::Object -> ObjectTypeDefinitionPosition);
fallible_conversions!(ObjectOrInterfaceTypeDefinitionPosition::Interface -> InterfaceTypeDefinitionPosition);
fallible_conversions!(TypeDefinitionPosition::{Object, Interface} -> ObjectOrInterfaceTypeDefinitionPosition);
fallible_conversions!(OutputTypeDefinitionPosition::{Object, Interface} -> ObjectOrInterfaceTypeDefinitionPosition);
fallible_conversions!(CompositeTypeDefinitionPosition::{Object, Interface} -> ObjectOrInterfaceTypeDefinitionPosition);
fallible_conversions!(AbstractTypeDefinitionPosition::{Interface} -> ObjectOrInterfaceTypeDefinitionPosition);

#[derive(Clone, PartialEq, Eq, Hash, derive_more::From, derive_more::Display, Serialize)]
pub(crate) enum FieldDefinitionPosition {
    Object(ObjectFieldDefinitionPosition),
    Interface(InterfaceFieldDefinitionPosition),
    Union(UnionTypenameFieldDefinitionPosition),
}

impl Debug for FieldDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Object(p) => write!(f, "Object({p})"),
            Self::Interface(p) => write!(f, "Interface({p})"),
            Self::Union(p) => write!(f, "Union({p})"),
        }
    }
}

impl FieldDefinitionPosition {
    pub(crate) fn type_name(&self) -> &Name {
        match self {
            FieldDefinitionPosition::Object(field) => &field.type_name,
            FieldDefinitionPosition::Interface(field) => &field.type_name,
            FieldDefinitionPosition::Union(field) => &field.type_name,
        }
    }

    pub(crate) fn field_name(&self) -> &Name {
        match self {
            FieldDefinitionPosition::Object(field) => &field.field_name,
            FieldDefinitionPosition::Interface(field) => &field.field_name,
            FieldDefinitionPosition::Union(field) => field.field_name(),
        }
    }

    pub(crate) fn is_introspection_typename_field(&self) -> bool {
        *self.field_name() == *INTROSPECTION_TYPENAME_FIELD_NAME
    }

    pub(crate) fn parent(&self) -> CompositeTypeDefinitionPosition {
        match self {
            FieldDefinitionPosition::Object(field) => field.parent().into(),
            FieldDefinitionPosition::Interface(field) => field.parent().into(),
            FieldDefinitionPosition::Union(field) => field.parent().into(),
        }
    }

    #[allow(unused)]
    pub(crate) fn has_applied_directive(
        &self,
        schema: &FederationSchema,
        directive_name: &Name,
    ) -> bool {
        match self {
            FieldDefinitionPosition::Object(field) => !field
                .get_applied_directives(schema, directive_name)
                .is_empty(),
            FieldDefinitionPosition::Interface(field) => !field
                .get_applied_directives(schema, directive_name)
                .is_empty(),
            FieldDefinitionPosition::Union(_) => false,
        }
    }

    pub(crate) fn get_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
        directive_name: &Name,
    ) -> Vec<&'schema Node<Directive>> {
        match self {
            FieldDefinitionPosition::Object(field) => {
                field.get_applied_directives(schema, directive_name)
            }
            FieldDefinitionPosition::Interface(field) => {
                field.get_applied_directives(schema, directive_name)
            }
            FieldDefinitionPosition::Union(_) => vec![],
        }
    }

    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Node<Directive>,
    ) {
        match self {
            FieldDefinitionPosition::Object(field) => field.remove_directive(schema, directive),
            FieldDefinitionPosition::Interface(field) => field.remove_directive(schema, directive),
            FieldDefinitionPosition::Union(_) => (),
        }
    }
    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Component<FieldDefinition>, PositionLookupError> {
        match self {
            FieldDefinitionPosition::Object(field) => field.get(schema),
            FieldDefinitionPosition::Interface(field) => field.get(schema),
            FieldDefinitionPosition::Union(field) => field.get(schema),
        }
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema Component<FieldDefinition>> {
        self.get(schema).ok()
    }

    pub(crate) fn is_interface(&self) -> bool {
        matches!(self, FieldDefinitionPosition::Interface(_))
    }
}

infallible_conversions!(ObjectOrInterfaceFieldDefinitionPosition::{Object, Interface} -> FieldDefinitionPosition);

impl TryFrom<DirectiveTargetPosition> for FieldDefinitionPosition {
    type Error = &'static str;

    fn try_from(dl: DirectiveTargetPosition) -> Result<Self, Self::Error> {
        match dl {
            DirectiveTargetPosition::ObjectField(field) => {
                Ok(FieldDefinitionPosition::Object(field))
            }
            DirectiveTargetPosition::InterfaceField(field) => {
                Ok(FieldDefinitionPosition::Interface(field))
            }
            _ => Err("No valid conversion"),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, derive_more::From, derive_more::Display)]
pub(crate) enum ObjectOrInterfaceFieldDefinitionPosition {
    Object(ObjectFieldDefinitionPosition),
    Interface(InterfaceFieldDefinitionPosition),
}

impl Debug for ObjectOrInterfaceFieldDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Object(p) => write!(f, "Object({p})"),
            Self::Interface(p) => write!(f, "Interface({p})"),
        }
    }
}

impl ObjectOrInterfaceFieldDefinitionPosition {
    const EXPECTED: &'static str = "an object/interface field";

    pub(crate) fn type_name(&self) -> &Name {
        match self {
            ObjectOrInterfaceFieldDefinitionPosition::Object(field) => &field.type_name,
            ObjectOrInterfaceFieldDefinitionPosition::Interface(field) => &field.type_name,
        }
    }

    pub(crate) fn field_name(&self) -> &Name {
        match self {
            ObjectOrInterfaceFieldDefinitionPosition::Object(field) => &field.field_name,
            ObjectOrInterfaceFieldDefinitionPosition::Interface(field) => &field.field_name,
        }
    }

    pub(crate) fn parent(&self) -> ObjectOrInterfaceTypeDefinitionPosition {
        match self {
            ObjectOrInterfaceFieldDefinitionPosition::Object(field) => field.parent().into(),
            ObjectOrInterfaceFieldDefinitionPosition::Interface(field) => field.parent().into(),
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Component<FieldDefinition>, PositionLookupError> {
        match self {
            ObjectOrInterfaceFieldDefinitionPosition::Object(field) => field.get(schema),
            ObjectOrInterfaceFieldDefinitionPosition::Interface(field) => field.get(schema),
        }
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema Component<FieldDefinition>> {
        self.get(schema).ok()
    }

    pub(crate) fn insert_directive(
        &self,
        schema: &mut FederationSchema,
        directive: Node<Directive>,
    ) -> Result<(), FederationError> {
        match self {
            ObjectOrInterfaceFieldDefinitionPosition::Object(field) => {
                field.insert_directive(schema, directive)
            }
            ObjectOrInterfaceFieldDefinitionPosition::Interface(field) => {
                field.insert_directive(schema, directive)
            }
        }
    }

    pub(crate) fn has_applied_directive(
        &self,
        schema: &FederationSchema,
        directive_name: &Name,
    ) -> bool {
        match self {
            Self::Object(field) => !field
                .get_applied_directives(schema, directive_name)
                .is_empty(),
            Self::Interface(field) => !field
                .get_applied_directives(schema, directive_name)
                .is_empty(),
        }
    }

    /// Remove a directive application from this field.
    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Node<Directive>,
    ) {
        match self {
            ObjectOrInterfaceFieldDefinitionPosition::Object(field) => {
                field.remove_directive(schema, directive)
            }
            ObjectOrInterfaceFieldDefinitionPosition::Interface(field) => {
                field.remove_directive(schema, directive)
            }
        }
    }

    pub(crate) fn coordinate(&self) -> String {
        format!("{}.{}", self.type_name(), self.field_name())
    }

    pub(crate) fn insert(
        &self,
        schema: &mut FederationSchema,
        field_def: Component<FieldDefinition>,
    ) -> Result<(), FederationError> {
        match self {
            Self::Object(field) => field.insert(schema, field_def),
            Self::Interface(field) => field.insert(schema, field_def),
        }
    }
}

fallible_conversions!(FieldDefinitionPosition::{Object, Interface} -> ObjectOrInterfaceFieldDefinitionPosition);

#[derive(Debug, Clone, PartialEq, Eq, Hash, derive_more::Display)]
pub(crate) struct SchemaDefinitionPosition;

impl SchemaDefinitionPosition {
    pub(crate) fn get_all_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> impl Iterator<Item = &'schema Component<Directive>> {
        self.get(&schema.schema).directives.iter()
    }

    pub(crate) fn get_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
        directive_name: &Name,
    ) -> Vec<&'schema Component<Directive>> {
        let schema_def = self.get(&schema.schema);
        schema_def
            .directives
            .iter()
            .filter(|d| d.name == *directive_name)
            .collect()
    }
    pub(crate) fn get<'schema>(&self, schema: &'schema Schema) -> &'schema Node<SchemaDefinition> {
        &schema.schema_definition
    }

    fn make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> &'schema mut Node<SchemaDefinition> {
        &mut schema.schema_definition
    }

    pub(crate) fn insert_directive(
        &self,
        schema: &mut FederationSchema,
        directive: Component<Directive>,
    ) -> Result<(), FederationError> {
        self.insert_directive_at(schema, directive, self.get(&schema.schema).directives.len())
    }

    pub(crate) fn insert_directive_at(
        &self,
        schema: &mut FederationSchema,
        directive: Component<Directive>,
        index: usize,
    ) -> Result<(), FederationError> {
        let schema_definition = self.make_mut(&mut schema.schema);
        if schema_definition
            .directives
            .iter()
            .any(|other_directive| other_directive.ptr_eq(&directive))
        {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Directive application \"@{}\" already exists on schema definition",
                    directive.name,
                ),
            }
            .into());
        }
        let name = directive.name.clone();
        schema_definition
            .make_mut()
            .directives
            .insert(index, directive);
        self.insert_directive_name_references(&mut schema.referencers, &name)?;
        schema.links_metadata = links_metadata(&schema.schema)?.map(Box::new);
        Ok(())
    }

    /// Remove directive applications with this name from the schema definition.
    pub(crate) fn remove_directive_name(
        &self,
        schema: &mut FederationSchema,
        name: &str,
    ) -> Result<(), FederationError> {
        let is_link = Self::is_link(schema, name)?;
        self.remove_directive_name_references(&mut schema.referencers, name);
        self.make_mut(&mut schema.schema)
            .make_mut()
            .directives
            .retain(|other_directive| other_directive.name != name);
        if is_link {
            schema.links_metadata = links_metadata(&schema.schema)?.map(Box::new);
        }
        Ok(())
    }

    fn insert_references(
        &self,
        schema_definition: &Node<SchemaDefinition>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        validate_component_directives(schema_definition.directives.deref())?;
        for directive_reference in schema_definition.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        for root_kind in SchemaRootDefinitionKind::iter() {
            let child = SchemaRootDefinitionPosition { root_kind };
            match root_kind {
                SchemaRootDefinitionKind::Query => {
                    if let Some(root_type) = &schema_definition.query {
                        child.insert_references(root_type, schema, referencers)?;
                    }
                }
                SchemaRootDefinitionKind::Mutation => {
                    if let Some(root_type) = &schema_definition.mutation {
                        child.insert_references(root_type, schema, referencers)?;
                    }
                }
                SchemaRootDefinitionKind::Subscription => {
                    if let Some(root_type) = &schema_definition.subscription {
                        child.insert_references(root_type, schema, referencers)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn insert_directive_name_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let directive_referencers = referencers.directives.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Schema definition's directive application \"@{name}\" does not refer to an existing directive.",
                ),
            }
        })?;
        directive_referencers.schema = Some(SchemaDefinitionPosition);
        Ok(())
    }

    fn remove_directive_name_references(&self, referencers: &mut Referencers, name: &str) {
        let Some(directive_referencers) = referencers.directives.get_mut(name) else {
            return;
        };
        directive_referencers.schema = None;
    }

    fn is_link(schema: &FederationSchema, name: &str) -> Result<bool, FederationError> {
        Ok(match schema.metadata() {
            Some(metadata) => {
                let link_spec_definition = metadata.link_spec_definition()?;
                let link_name_in_schema = link_spec_definition
                    .directive_name_in_schema(schema, &link_spec_definition.identity().name)?
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: "Unexpectedly could not find core/link spec usage".to_owned(),
                    })?;
                link_name_in_schema == name
            }
            None => false,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Hash, derive_more::From, derive_more::Display)]
pub(crate) enum TagDirectiveTargetPosition {
    ObjectField(ObjectFieldDefinitionPosition),
    InterfaceField(InterfaceFieldDefinitionPosition),
    UnionField(UnionTypenameFieldDefinitionPosition),
    Object(ObjectTypeDefinitionPosition),
    Interface(InterfaceTypeDefinitionPosition),
    Union(UnionTypeDefinitionPosition),
    ArgumentDefinition(FieldArgumentDefinitionPosition),
    Scalar(ScalarTypeDefinitionPosition),
    Enum(EnumTypeDefinitionPosition),
    EnumValue(EnumValueDefinitionPosition),
    InputObject(InputObjectTypeDefinitionPosition),
    InputObjectFieldDefinition(InputObjectFieldDefinitionPosition),
    Schema(SchemaDefinitionPosition),
    DirectiveArgumentDefinition(DirectiveArgumentDefinitionPosition),
}

impl Debug for TagDirectiveTargetPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ObjectField(p) => write!(f, "ObjectField({p})"),
            Self::InterfaceField(p) => write!(f, "InterfaceField({p})"),
            Self::UnionField(p) => write!(f, "UnionField({p})"),
            Self::Object(p) => write!(f, "Object({p})"),
            Self::Interface(p) => write!(f, "Interface({p})"),
            Self::Union(p) => write!(f, "Union({p})"),
            Self::ArgumentDefinition(p) => write!(f, "ArgumentDefinition({p})"),
            Self::Scalar(p) => write!(f, "Scalar({p})"),
            Self::Enum(p) => write!(f, "Enum({p})"),
            Self::EnumValue(p) => write!(f, "EnumValue({p})"),
            Self::InputObject(p) => write!(f, "InputObject({p})"),
            Self::InputObjectFieldDefinition(p) => write!(f, "InputObjectFieldDefinition({p})"),
            Self::Schema(p) => write!(f, "Schema({p})"),
            Self::DirectiveArgumentDefinition(p) => {
                write!(f, "DirectiveArgumentDefinition({p})")
            }
        }
    }
}

fallible_conversions!(TagDirectiveTargetPosition::Object -> ObjectTypeDefinitionPosition);
fallible_conversions!(TagDirectiveTargetPosition::Interface -> InterfaceTypeDefinitionPosition);
fallible_conversions!(TagDirectiveTargetPosition::Union -> UnionTypeDefinitionPosition);
fallible_conversions!(TagDirectiveTargetPosition::Scalar -> ScalarTypeDefinitionPosition);
fallible_conversions!(TagDirectiveTargetPosition::Enum -> EnumTypeDefinitionPosition);
fallible_conversions!(TagDirectiveTargetPosition::InputObject -> InputObjectTypeDefinitionPosition);

impl TryFrom<TagDirectiveTargetPosition> for FieldDefinitionPosition {
    type Error = &'static str;

    fn try_from(dl: TagDirectiveTargetPosition) -> Result<Self, Self::Error> {
        match dl {
            TagDirectiveTargetPosition::ObjectField(field) => {
                Ok(FieldDefinitionPosition::Object(field))
            }
            TagDirectiveTargetPosition::InterfaceField(field) => {
                Ok(FieldDefinitionPosition::Interface(field))
            }
            TagDirectiveTargetPosition::UnionField(field) => {
                Ok(FieldDefinitionPosition::Union(field))
            }
            _ => Err("No valid conversion"),
        }
    }
}

#[derive(
    Debug,
    Copy,
    Clone,
    PartialEq,
    Eq,
    Hash,
    strum_macros::Display,
    strum_macros::EnumIter,
    Serialize,
)]
pub(crate) enum SchemaRootDefinitionKind {
    #[strum(to_string = "query")]
    Query,
    #[strum(to_string = "mutation")]
    Mutation,
    #[strum(to_string = "subscription")]
    Subscription,
}

impl From<SchemaRootDefinitionKind> for ast::OperationType {
    fn from(value: SchemaRootDefinitionKind) -> Self {
        match value {
            SchemaRootDefinitionKind::Query => ast::OperationType::Query,
            SchemaRootDefinitionKind::Mutation => ast::OperationType::Mutation,
            SchemaRootDefinitionKind::Subscription => ast::OperationType::Subscription,
        }
    }
}

impl From<ast::OperationType> for SchemaRootDefinitionKind {
    fn from(value: ast::OperationType) -> Self {
        match value {
            ast::OperationType::Query => SchemaRootDefinitionKind::Query,
            ast::OperationType::Mutation => SchemaRootDefinitionKind::Mutation,
            ast::OperationType::Subscription => SchemaRootDefinitionKind::Subscription,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SchemaRootDefinitionPosition {
    pub(crate) root_kind: SchemaRootDefinitionKind,
}

impl SchemaRootDefinitionPosition {
    pub(crate) fn parent(&self) -> SchemaDefinitionPosition {
        SchemaDefinitionPosition
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema ComponentName, FederationError> {
        let schema_definition = self.parent().get(schema);

        match self.root_kind {
            SchemaRootDefinitionKind::Query => schema_definition.query.as_ref().ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Schema definition has no root {self} type"),
                }
                .into()
            }),
            SchemaRootDefinitionKind::Mutation => {
                schema_definition.mutation.as_ref().ok_or_else(|| {
                    SingleFederationError::Internal {
                        message: format!("Schema definition has no root {self} type"),
                    }
                    .into()
                })
            }
            SchemaRootDefinitionKind::Subscription => {
                schema_definition.subscription.as_ref().ok_or_else(|| {
                    SingleFederationError::Internal {
                        message: format!("Schema definition has no root {self} type"),
                    }
                    .into()
                })
            }
        }
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema ComponentName> {
        self.get(schema).ok()
    }

    pub(crate) fn insert(
        &self,
        schema: &mut FederationSchema,
        root_type: ComponentName,
    ) -> Result<(), FederationError> {
        if self.try_get(&schema.schema).is_some() {
            return Err(SingleFederationError::Internal {
                message: format!("Root {self} already exists on schema definition"),
            }
            .into());
        }
        let parent = self.parent().make_mut(&mut schema.schema).make_mut();
        match self.root_kind {
            SchemaRootDefinitionKind::Query => {
                parent.query = Some(root_type);
            }
            SchemaRootDefinitionKind::Mutation => {
                parent.mutation = Some(root_type);
            }
            SchemaRootDefinitionKind::Subscription => {
                parent.subscription = Some(root_type);
            }
        }
        self.insert_references(
            self.get(&schema.schema)?,
            &schema.schema,
            &mut schema.referencers,
        )
    }

    /// Remove this root definition from the schema.
    pub(crate) fn remove(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let Some(root_type) = self.try_get(&schema.schema) else {
            return Ok(());
        };
        self.remove_references(root_type, &schema.schema, &mut schema.referencers)?;
        let parent = self.parent().make_mut(&mut schema.schema).make_mut();
        match self.root_kind {
            SchemaRootDefinitionKind::Query => {
                parent.query = None;
            }
            SchemaRootDefinitionKind::Mutation => {
                parent.mutation = None;
            }
            SchemaRootDefinitionKind::Subscription => {
                parent.subscription = None;
            }
        }
        Ok(())
    }

    fn insert_references(
        &self,
        root_type: &ComponentName,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let object_type_referencers = referencers
            .object_types
            .get_mut(root_type.deref())
            .ok_or_else(|| SingleFederationError::Internal {
                message: format!(
                    "Root {} type \"{}\" does not refer to an existing object type.",
                    self,
                    root_type.deref()
                ),
            })?;
        object_type_referencers.schema_roots.insert(self.clone());
        if self.root_kind == SchemaRootDefinitionKind::Query {
            ObjectTypeDefinitionPosition {
                type_name: root_type.name.clone(),
            }
            .insert_root_query_references(schema, referencers)?;
        }
        Ok(())
    }

    fn remove_references(
        &self,
        root_type: &ComponentName,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if self.root_kind == SchemaRootDefinitionKind::Query {
            ObjectTypeDefinitionPosition {
                type_name: root_type.name.clone(),
            }
            .remove_root_query_references(schema, referencers)?;
        }
        let Some(object_type_referencers) = referencers.object_types.get_mut(root_type.deref())
        else {
            return Ok(());
        };
        object_type_referencers.schema_roots.shift_remove(self);
        Ok(())
    }

    fn rename_type(
        &self,
        schema: &mut FederationSchema,
        new_name: Name,
    ) -> Result<(), FederationError> {
        let parent = self.parent().make_mut(&mut schema.schema).make_mut();
        match self.root_kind {
            SchemaRootDefinitionKind::Query => {
                if let Some(query) = &mut parent.query {
                    query.name = new_name;
                }
            }
            SchemaRootDefinitionKind::Mutation => {
                if let Some(mutation) = &mut parent.mutation {
                    mutation.name = new_name;
                }
            }
            SchemaRootDefinitionKind::Subscription => {
                if let Some(subscription) = &mut parent.subscription {
                    subscription.name = new_name;
                }
            }
        }
        Ok(())
    }
}

impl Display for SchemaRootDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.root_kind)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ScalarTypeDefinitionPosition {
    pub(crate) type_name: Name,
}

impl ScalarTypeDefinitionPosition {
    const EXPECTED: &'static str = "a scalar type";

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<ScalarType>, PositionLookupError> {
        schema
            .types
            .get(&self.type_name)
            .ok_or_else(|| PositionLookupError::TypeMissing(self.type_name.clone()))
            .and_then(|type_| {
                if let ExtendedType::Scalar(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(PositionLookupError::TypeWrongKind(
                        self.type_name.clone(),
                        Self::EXPECTED,
                    ))
                }
            })
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema Node<ScalarType>> {
        self.get(schema).ok()
    }

    fn make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Result<&'schema mut Node<ScalarType>, PositionLookupError> {
        schema
            .types
            .get_mut(&self.type_name)
            .ok_or_else(|| PositionLookupError::TypeMissing(self.type_name.clone()))
            .and_then(|type_| {
                if let ExtendedType::Scalar(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(PositionLookupError::TypeWrongKind(
                        self.type_name.clone(),
                        Self::EXPECTED,
                    ))
                }
            })
    }

    fn try_make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Option<&'schema mut Node<ScalarType>> {
        if self.try_get(schema).is_some() {
            self.make_mut(schema).ok()
        } else {
            None
        }
    }

    pub(crate) fn pre_insert(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        if schema.referencers.contains_type_name(&self.type_name) {
            bail!(r#"Type "{self}" has already been pre-inserted"#);
        }
        schema
            .referencers
            .scalar_types
            .insert(self.type_name.clone(), Default::default());
        Ok(())
    }

    pub(crate) fn insert(
        &self,
        schema: &mut FederationSchema,
        type_: Node<ScalarType>,
    ) -> Result<(), FederationError> {
        if self.type_name != type_.name {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Scalar type \"{}\" given type named \"{}\"",
                    self, type_.name,
                ),
            }
            .into());
        }
        if !schema
            .referencers
            .scalar_types
            .contains_key(&self.type_name)
        {
            bail!(r#"Type "{self}" has not been pre-inserted"#);
        }
        if schema.schema.types.contains_key(&self.type_name) {
            bail!(r#"Type "{self}" already exists in schema"#);
        }
        schema
            .schema
            .types
            .insert(self.type_name.clone(), ExtendedType::Scalar(type_));
        self.insert_references(self.get(&schema.schema)?, &mut schema.referencers)
    }

    /// Remove this scalar type from the schema. Also remove any fields or arguments that directly reference this type.
    pub(crate) fn remove(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<Option<ScalarTypeReferencers>, FederationError> {
        let Some(referencers) = self.remove_internal(schema)? else {
            return Ok(None);
        };
        for field in &referencers.object_fields {
            field.remove(schema)?;
        }
        for argument in &referencers.object_field_arguments {
            argument.remove(schema)?;
        }
        for field in &referencers.interface_fields {
            field.remove(schema)?;
        }
        for argument in &referencers.interface_field_arguments {
            argument.remove(schema)?;
        }
        for field in &referencers.input_object_fields {
            field.remove(schema)?;
        }
        for argument in &referencers.directive_arguments {
            argument.remove(schema)?;
        }
        Ok(Some(referencers))
    }

    fn remove_internal(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<Option<ScalarTypeReferencers>, FederationError> {
        let Some(type_) = self.try_get(&schema.schema) else {
            return Ok(None);
        };
        self.remove_references(type_, &mut schema.referencers);
        schema.schema.types.shift_remove(&self.type_name);
        Ok(Some(
            schema
                .referencers
                .scalar_types
                .shift_remove(&self.type_name)
                .ok_or_else(|| SingleFederationError::Internal {
                    message: format!("Schema missing referencers for type \"{self}\""),
                })?,
        ))
    }

    pub(crate) fn insert_directive(
        &self,
        schema: &mut FederationSchema,
        directive: Component<Directive>,
    ) -> Result<(), FederationError> {
        let type_ = self.make_mut(&mut schema.schema)?;
        if type_
            .directives
            .iter()
            .any(|other_directive| other_directive.ptr_eq(&directive))
        {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Directive application \"@{}\" already exists on scalar type \"{}\"",
                    directive.name, self,
                ),
            }
            .into());
        }
        let name = directive.name.clone();
        type_.make_mut().directives.push(directive);
        self.insert_directive_name_references(&mut schema.referencers, &name)
    }

    /// Remove a directive application from this position by name.
    pub(crate) fn remove_directive_name(&self, schema: &mut FederationSchema, name: &str) {
        let Some(type_) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        self.remove_directive_name_references(&mut schema.referencers, name);
        type_
            .make_mut()
            .directives
            .retain(|other_directive| other_directive.name != name);
    }

    fn insert_references(
        &self,
        type_: &Node<ScalarType>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        validate_component_directives(type_.directives.deref())?;
        for directive_reference in type_.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        Ok(())
    }

    fn remove_references(&self, type_: &Node<ScalarType>, referencers: &mut Referencers) {
        for directive_reference in type_.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
    }

    fn insert_directive_name_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let directive_referencers = referencers.directives.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Scalar type \"{self}\"'s directive application \"@{name}\" does not refer to an existing directive.",
                ),
            }
        })?;
        directive_referencers.scalar_types.insert(self.clone());
        Ok(())
    }

    fn remove_directive_name_references(&self, referencers: &mut Referencers, name: &str) {
        let Some(directive_referencers) = referencers.directives.get_mut(name) else {
            return;
        };
        directive_referencers.scalar_types.shift_remove(self);
    }

    fn rename(&self, schema: &mut FederationSchema, new_name: Name) -> Result<(), FederationError> {
        self.make_mut(&mut schema.schema)?.make_mut().name = new_name.clone();

        if let Some(scalar_type_referencers) =
            schema.referencers.scalar_types.swap_remove(&self.type_name)
        {
            for pos in scalar_type_referencers.object_fields.iter() {
                pos.rename_type(schema, new_name.clone())?;
            }
            for pos in scalar_type_referencers.object_field_arguments.iter() {
                pos.rename_type(schema, new_name.clone())?;
            }
            for pos in scalar_type_referencers.interface_fields.iter() {
                pos.rename_type(schema, new_name.clone())?;
            }
            for pos in scalar_type_referencers.interface_field_arguments.iter() {
                pos.rename_type(schema, new_name.clone())?;
            }
            for pos in scalar_type_referencers.input_object_fields.iter() {
                pos.rename_type(schema, new_name.clone())?;
            }
            schema
                .referencers
                .scalar_types
                .insert(new_name, scalar_type_referencers);
        }

        Ok(())
    }

    fn remove_extensions(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        for directive in self
            .make_mut(&mut schema.schema)?
            .make_mut()
            .directives
            .iter_mut()
        {
            directive.origin = ComponentOrigin::Definition;
        }
        Ok(())
    }

    pub(crate) fn has_applied_directive(
        &self,
        schema: &FederationSchema,
        directive_name: &Name,
    ) -> bool {
        if let Some(type_) = self.try_get(schema.schema()) {
            return type_
                .directives
                .iter()
                .any(|directive| &directive.name == directive_name);
        }
        false
    }

    pub(crate) fn get_all_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<impl Iterator<Item = &'schema Component<Directive>>, FederationError> {
        Ok(self.get(&schema.schema)?.directives.iter())
    }

    pub(crate) fn get_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
        directive_name: &Name,
    ) -> Vec<&'schema Component<Directive>> {
        if let Some(field) = self.try_get(&schema.schema) {
            field
                .directives
                .iter()
                .filter(|directive| &directive.name == directive_name)
                .collect()
        } else {
            Vec::new()
        }
    }

    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Component<Directive>,
    ) {
        let Some(obj) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        if !obj.directives.iter().any(|other_directive| {
            (other_directive.name == directive.name) && !other_directive.ptr_eq(directive)
        }) {
            self.remove_directive_name_references(&mut schema.referencers, &directive.name);
        }
        obj.make_mut()
            .directives
            .retain(|other_directive| !other_directive.ptr_eq(directive));
    }
}

impl Display for ScalarTypeDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.type_name)
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
pub(crate) struct ObjectTypeDefinitionPosition {
    pub(crate) type_name: Name,
}

impl ObjectTypeDefinitionPosition {
    const EXPECTED: &'static str = "an object type";

    pub(crate) fn new(type_name: Name) -> Self {
        Self { type_name }
    }

    pub(crate) fn field(&self, field_name: Name) -> ObjectFieldDefinitionPosition {
        ObjectFieldDefinitionPosition {
            type_name: self.type_name.clone(),
            field_name,
        }
    }

    pub(crate) fn introspection_typename_field(&self) -> ObjectFieldDefinitionPosition {
        self.field(INTROSPECTION_TYPENAME_FIELD_NAME.clone())
    }

    pub(crate) fn introspection_schema_field(&self) -> ObjectFieldDefinitionPosition {
        self.field(name!("__schema"))
    }

    pub(crate) fn introspection_type_field(&self) -> ObjectFieldDefinitionPosition {
        self.field(name!("__type"))
    }

    pub(crate) fn fields<'a>(
        &'a self,
        schema: &'a Schema,
    ) -> Result<impl Iterator<Item = ObjectFieldDefinitionPosition>, FederationError> {
        Ok(self
            .get(schema)?
            .fields
            .keys()
            .map(|name| self.field(name.clone())))
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<ObjectType>, PositionLookupError> {
        schema
            .types
            .get(&self.type_name)
            .ok_or_else(|| PositionLookupError::TypeMissing(self.type_name.clone()))
            .and_then(|type_| {
                if let ExtendedType::Object(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(PositionLookupError::TypeWrongKind(
                        self.type_name.clone(),
                        Self::EXPECTED,
                    ))
                }
            })
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema Node<ObjectType>> {
        self.get(schema).ok()
    }

    pub(crate) fn make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Result<&'schema mut Node<ObjectType>, PositionLookupError> {
        schema
            .types
            .get_mut(&self.type_name)
            .ok_or_else(|| PositionLookupError::TypeMissing(self.type_name.clone()))
            .and_then(|type_| {
                if let ExtendedType::Object(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(PositionLookupError::TypeWrongKind(
                        self.type_name.clone(),
                        Self::EXPECTED,
                    ))
                }
            })
    }

    fn try_make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Option<&'schema mut Node<ObjectType>> {
        if self.try_get(schema).is_some() {
            self.make_mut(schema).ok()
        } else {
            None
        }
    }

    pub(crate) fn pre_insert(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        if schema.referencers.contains_type_name(&self.type_name) {
            bail!(r#"Type "{self}" has already been pre-inserted"#);
        }
        schema
            .referencers
            .object_types
            .insert(self.type_name.clone(), Default::default());
        Ok(())
    }

    pub(crate) fn insert(
        &self,
        schema: &mut FederationSchema,
        type_: Node<ObjectType>,
    ) -> Result<(), FederationError> {
        if self.type_name != type_.name {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Object type \"{}\" given type named \"{}\"",
                    self, type_.name,
                ),
            }
            .into());
        }
        if !schema
            .referencers
            .object_types
            .contains_key(&self.type_name)
        {
            bail!(r#"Type "{self}" has not been pre-inserted"#);
        }
        if schema.schema.types.contains_key(&self.type_name) {
            bail!(r#"Type "{self}" already exists in schema"#);
        }
        schema
            .schema
            .types
            .insert(self.type_name.clone(), ExtendedType::Object(type_));
        self.insert_references(
            self.get(&schema.schema)?,
            &schema.schema,
            &mut schema.referencers,
        )
    }

    /// Remove the type from the schema, and remove any direct references to the type.
    ///
    /// This may make the schema invalid if a reference to the type is the only element inside the
    /// reference's type: for example if `self` is the only member of a union `U`, `U` will become
    /// empty, and thus invalid.
    pub(crate) fn remove(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<Option<ObjectTypeReferencers>, FederationError> {
        let Some(referencers) = self.remove_internal(schema)? else {
            return Ok(None);
        };
        for root in &referencers.schema_roots {
            root.remove(schema)?;
        }
        for field in &referencers.object_fields {
            field.remove(schema)?;
        }
        for field in &referencers.interface_fields {
            field.remove(schema)?;
        }
        for type_ in &referencers.union_types {
            type_.remove_member(schema, &self.type_name);
        }
        Ok(Some(referencers))
    }

    /// Remove the type from the schema, and recursively remove any references to the type.
    pub(crate) fn remove_recursive(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        let Some(referencers) = self.remove_internal(schema)? else {
            return Ok(());
        };
        for root in referencers.schema_roots {
            root.remove(schema)?;
        }
        for field in referencers.object_fields {
            field.remove_recursive(schema)?;
        }
        for field in referencers.interface_fields {
            field.remove_recursive(schema)?;
        }
        for type_ in referencers.union_types {
            type_.remove_member_recursive(schema, &self.type_name)?;
        }
        Ok(())
    }

    fn remove_internal(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<Option<ObjectTypeReferencers>, FederationError> {
        let Some(type_) = self.try_get(&schema.schema) else {
            return Ok(None);
        };
        self.remove_references(type_, &schema.schema, &mut schema.referencers)?;
        schema.schema.types.shift_remove(&self.type_name);
        Ok(Some(
            schema
                .referencers
                .object_types
                .shift_remove(&self.type_name)
                .ok_or_else(|| SingleFederationError::Internal {
                    message: format!("Schema missing referencers for type \"{self}\""),
                })?,
        ))
    }

    pub(crate) fn insert_directive(
        &self,
        schema: &mut FederationSchema,
        directive: Component<Directive>,
    ) -> Result<(), FederationError> {
        let type_ = self.make_mut(&mut schema.schema)?;
        if type_
            .directives
            .iter()
            .any(|other_directive| other_directive.ptr_eq(&directive))
        {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Directive application \"@{}\" already exists on object type \"{}\"",
                    directive.name, self,
                ),
            }
            .into());
        }
        let name = directive.name.clone();
        type_.make_mut().directives.push(directive);
        self.insert_directive_name_references(&mut schema.referencers, &name)
    }

    /// Remove a directive application from this position by name.
    pub(crate) fn remove_directive_name(&self, schema: &mut FederationSchema, name: &str) {
        let Some(type_) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        self.remove_directive_name_references(&mut schema.referencers, name);
        type_
            .make_mut()
            .directives
            .retain(|other_directive| other_directive.name != name);
    }

    pub(crate) fn insert_implements_interface(
        &self,
        schema: &mut FederationSchema,
        name: ComponentName,
    ) -> Result<(), FederationError> {
        let type_ = self.make_mut(&mut schema.schema)?;
        type_.make_mut().implements_interfaces.insert(name.clone());
        self.insert_implements_interface_references(&mut schema.referencers, &name)
    }

    pub(crate) fn remove_implements_interface(&self, schema: &mut FederationSchema, name: &str) {
        let Some(type_) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        self.remove_implements_interface_references(&mut schema.referencers, name);
        type_
            .make_mut()
            .implements_interfaces
            .retain(|other_type| other_type != name);
    }

    fn insert_references(
        &self,
        type_: &Node<ObjectType>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        validate_component_directives(type_.directives.deref())?;
        for directive_reference in type_.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        for interface_type_reference in type_.implements_interfaces.iter() {
            self.insert_implements_interface_references(
                referencers,
                interface_type_reference.deref(),
            )?;
        }
        let introspection_typename_field = self.introspection_typename_field();
        introspection_typename_field.insert_references(
            introspection_typename_field.get(schema)?,
            referencers,
            true,
        )?;
        if let Some(root_query_type) = (SchemaRootDefinitionPosition {
            root_kind: SchemaRootDefinitionKind::Query,
        })
        .try_get(schema)
        {
            // Note that when inserting an object type that's the root query type, it's possible for
            // the root query type to have been set before this insertion. During that set, while
            // we would call insert_root_query_references(), it would ultimately do nothing since
            // the meta-fields wouldn't be found (since the type has only been pre-inserted at that
            // point, not fully inserted). We instead need to execute the reference insertion here,
            // as it's right after the type has been inserted.
            if self.type_name == root_query_type.name {
                self.insert_root_query_references(schema, referencers)?;
            }
        }
        for (field_name, field) in type_.fields.iter() {
            self.field(field_name.clone())
                .insert_references(field, referencers, false)?;
        }
        Ok(())
    }

    fn remove_references(
        &self,
        type_: &Node<ObjectType>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        for directive_reference in type_.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
        for interface_type_reference in type_.implements_interfaces.iter() {
            self.remove_implements_interface_references(
                referencers,
                interface_type_reference.deref(),
            );
        }
        let introspection_typename_field = self.introspection_typename_field();
        introspection_typename_field.remove_references(
            introspection_typename_field.get(schema)?,
            referencers,
            true,
        )?;
        if let Some(root_query_type) = (SchemaRootDefinitionPosition {
            root_kind: SchemaRootDefinitionKind::Query,
        })
        .try_get(schema)
        {
            // Note that when removing an object type that's the root query type, it will eventually
            // call SchemaRootDefinitionPosition.remove() to unset the root query type, and there's
            // code there to call remove_root_query_references(). However, that code won't find the
            // meta-fields __schema or __type, as the type has already been removed from the schema
            // before it executes. We instead need to execute the reference removal here, as it's
            // right before the type has been removed.
            if self.type_name == root_query_type.name {
                self.remove_root_query_references(schema, referencers)?;
            }
        }
        for (field_name, field) in type_.fields.iter() {
            self.field(field_name.clone())
                .remove_references(field, referencers, false)?;
        }
        Ok(())
    }

    fn insert_directive_name_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let directive_referencers = referencers.directives.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Object type \"{self}\"'s directive application \"@{name}\" does not refer to an existing directive.",
                ),
            }
        })?;
        directive_referencers.object_types.insert(self.clone());
        Ok(())
    }

    fn remove_directive_name_references(&self, referencers: &mut Referencers, name: &str) {
        let Some(directive_referencers) = referencers.directives.get_mut(name) else {
            return;
        };
        directive_referencers.object_types.shift_remove(self);
    }

    fn insert_implements_interface_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let interface_type_referencers = referencers.interface_types.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Object type \"{self}\"'s implements \"{name}\" does not refer to an existing interface.",
                ),
            }
        })?;
        interface_type_referencers.object_types.insert(self.clone());
        Ok(())
    }

    fn remove_implements_interface_references(&self, referencers: &mut Referencers, name: &str) {
        let Some(interface_type_referencers) = referencers.interface_types.get_mut(name) else {
            return;
        };
        interface_type_referencers.object_types.shift_remove(self);
    }

    fn insert_root_query_references(
        &self,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        // Note that unlike most insert logic in this file, the underlying elements being inserted
        // here (the meta-fields __schema and __type) actually depend on two elements existing
        // instead of one: the object type, and the schema root query type. This code is called at
        // insertions for both of those elements, but needs to be able to handle if one doesn't
        // exist, so accordingly we don't use get() below/we don't error if try_get() returns None.
        let introspection_schema_field = self.introspection_schema_field();
        if let Some(field) = introspection_schema_field.try_get(schema) {
            introspection_schema_field.insert_references(field, referencers, true)?;
        }
        let introspection_type_field = self.introspection_type_field();
        if let Some(field) = introspection_type_field.try_get(schema) {
            introspection_type_field.insert_references(field, referencers, true)?;
        }
        Ok(())
    }

    fn remove_root_query_references(
        &self,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let introspection_schema_field = self.introspection_schema_field();
        if let Some(field) = introspection_schema_field.try_get(schema) {
            introspection_schema_field.remove_references(field, referencers, true)?;
        }
        let introspection_type_field = self.introspection_type_field();
        if let Some(field) = introspection_type_field.try_get(schema) {
            introspection_type_field.remove_references(field, referencers, true)?;
        }
        Ok(())
    }

    fn rename(&self, schema: &mut FederationSchema, new_name: Name) -> Result<(), FederationError> {
        self.make_mut(&mut schema.schema)?.make_mut().name = new_name.clone();

        if let Some(object_type_referencers) =
            schema.referencers.object_types.swap_remove(&self.type_name)
        {
            for pos in object_type_referencers.schema_roots.iter() {
                pos.rename_type(schema, new_name.clone())?;
            }
            for pos in object_type_referencers.object_fields.iter() {
                pos.rename_type(schema, new_name.clone())?;
            }
            for pos in object_type_referencers.interface_fields.iter() {
                pos.rename_type(schema, new_name.clone())?;
            }
            for pos in object_type_referencers.union_types.iter() {
                pos.rename_member(schema, &self.type_name, new_name.clone())?;
            }

            schema
                .referencers
                .object_types
                .insert(new_name, object_type_referencers);
        }

        Ok(())
    }

    fn rename_implemented_interface(
        &self,
        schema: &mut FederationSchema,
        old_name: &Name,
        new_name: Name,
    ) -> Result<(), FederationError> {
        let type_ = self.make_mut(&mut schema.schema)?.make_mut();
        type_.implements_interfaces.swap_remove(old_name);
        type_.implements_interfaces.insert(new_name.into());
        Ok(())
    }

    fn remove_extensions(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let type_ = self.make_mut(&mut schema.schema)?.make_mut();
        for directive in type_.directives.iter_mut() {
            directive.origin = ComponentOrigin::Definition;
        }
        type_.implements_interfaces = type_
            .implements_interfaces
            .iter()
            .map(|i| {
                let mut i = i.clone();
                i.origin = ComponentOrigin::Definition;
                i
            })
            .collect();
        for (_, field) in type_.fields.iter_mut() {
            field.origin = ComponentOrigin::Definition;
        }
        Ok(())
    }

    pub(crate) fn has_applied_directive(
        &self,
        schema: &FederationSchema,
        directive_name: &Name,
    ) -> bool {
        if let Some(type_) = self.try_get(schema.schema()) {
            return type_
                .directives
                .iter()
                .any(|directive| &directive.name == directive_name);
        }
        false
    }

    pub(crate) fn get_all_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<impl Iterator<Item = &'schema Component<Directive>>, FederationError> {
        Ok(self.get(&schema.schema)?.directives.iter())
    }

    pub(crate) fn get_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
        directive_name: &Name,
    ) -> Vec<&'schema Component<Directive>> {
        if let Some(field) = self.try_get(&schema.schema) {
            field
                .directives
                .iter()
                .filter(|directive| &directive.name == directive_name)
                .collect()
        } else {
            Vec::new()
        }
    }

    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Component<Directive>,
    ) {
        let Some(obj) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        if !obj.directives.iter().any(|other_directive| {
            (other_directive.name == directive.name) && !other_directive.ptr_eq(directive)
        }) {
            self.remove_directive_name_references(&mut schema.referencers, &directive.name);
        }
        obj.make_mut()
            .directives
            .retain(|other_directive| !other_directive.ptr_eq(directive));
    }
}

impl Display for ObjectTypeDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.type_name)
    }
}

impl Debug for ObjectTypeDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Object({self})")
    }
}

#[derive(Clone, PartialEq, Eq, Hash, derive_more::From, derive_more::Display)]
pub(crate) enum FieldArgumentDefinitionPosition {
    Interface(InterfaceFieldArgumentDefinitionPosition),
    Object(ObjectFieldArgumentDefinitionPosition),
}

impl FieldArgumentDefinitionPosition {
    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<InputValueDefinition>, PositionLookupError> {
        match self {
            Self::Interface(p) => p.get(schema),
            Self::Object(p) => p.get(schema),
        }
    }

    pub(crate) fn make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Result<&'schema mut Node<InputValueDefinition>, PositionLookupError> {
        match self {
            Self::Interface(p) => p.make_mut(schema),
            Self::Object(p) => p.make_mut(schema),
        }
    }
}

impl Debug for FieldArgumentDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Interface(p) => write!(f, "Interface({p})"),
            Self::Object(p) => write!(f, "Object({p})"),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
pub(crate) struct ObjectFieldDefinitionPosition {
    pub(crate) type_name: Name,
    pub(crate) field_name: Name,
}

impl ObjectFieldDefinitionPosition {
    pub(crate) fn parent(&self) -> ObjectTypeDefinitionPosition {
        ObjectTypeDefinitionPosition {
            type_name: self.type_name.clone(),
        }
    }

    pub(crate) fn argument(&self, argument_name: Name) -> ObjectFieldArgumentDefinitionPosition {
        ObjectFieldArgumentDefinitionPosition {
            type_name: self.type_name.clone(),
            field_name: self.field_name.clone(),
            argument_name,
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Component<FieldDefinition>, PositionLookupError> {
        let parent = self.parent();
        parent.get(schema)?;

        schema
            .type_field(&self.type_name, &self.field_name)
            .map_err(|_| {
                PositionLookupError::MissingField(
                    "Object",
                    self.type_name.clone(),
                    self.field_name.clone(),
                )
            })
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema Component<FieldDefinition>> {
        self.get(schema).ok()
    }

    pub(crate) fn make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Result<&'schema mut Component<FieldDefinition>, PositionLookupError> {
        let parent = self.parent();
        let type_ = parent.make_mut(schema)?.make_mut();

        if is_graphql_reserved_name(&self.field_name) {
            return Err(PositionLookupError::MutateReservedField(
                "object field",
                self.type_name.clone(),
                self.field_name.clone(),
            ));
        }
        type_.fields.get_mut(&self.field_name).ok_or_else(|| {
            PositionLookupError::MissingField(
                "Object",
                self.type_name.clone(),
                self.field_name.clone(),
            )
        })
    }

    fn try_make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Option<&'schema mut Component<FieldDefinition>> {
        if self.try_get(schema).is_some() {
            self.make_mut(schema).ok()
        } else {
            None
        }
    }

    pub(crate) fn insert(
        &self,
        schema: &mut FederationSchema,
        field: Component<FieldDefinition>,
    ) -> Result<(), FederationError> {
        if self.field_name != field.name {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Object field \"{}\" given field named \"{}\"",
                    self, field.name,
                ),
            }
            .into());
        }
        if self.try_get(&schema.schema).is_some() {
            bail!(r#"Object field "{self}" already exists in schema"#);
        }
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .fields
            .insert(self.field_name.clone(), field);
        self.insert_references(self.get(&schema.schema)?, &mut schema.referencers, false)?;
        Ok(())
    }

    /// Remove the field from its type.
    ///
    /// This may make the schema invalid if the field is part of an interface declared by the type,
    /// or if this is the only field in a type.
    pub(crate) fn remove(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let Some(field) = self.try_get(&schema.schema) else {
            return Ok(());
        };
        self.remove_references(field, &mut schema.referencers, false)?;
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .fields
            .shift_remove(&self.field_name);
        Ok(())
    }

    /// Remove the field from its type. If the type becomes empty, remove the type as well.
    ///
    /// This may make the schema invalid if the field is part of an interface declared by the type.
    pub(crate) fn remove_recursive(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        self.remove(schema)?;
        let parent = self.parent();
        let Some(type_) = parent.try_get(&schema.schema) else {
            return Ok(());
        };
        if type_.fields.is_empty() {
            parent.remove_recursive(schema)?;
        }
        Ok(())
    }

    pub(crate) fn insert_directive(
        &self,
        schema: &mut FederationSchema,
        directive: Node<Directive>,
    ) -> Result<(), FederationError> {
        let field = self.make_mut(&mut schema.schema)?;
        if field
            .directives
            .iter()
            .any(|other_directive| other_directive.ptr_eq(&directive))
        {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Directive application \"@{}\" already exists on object field \"{}\"",
                    directive.name, self,
                ),
            }
            .into());
        }
        let name = directive.name.clone();
        field.make_mut().directives.push(directive);
        self.insert_directive_name_references(&mut schema.referencers, &name)
    }

    pub(crate) fn get_all_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<impl Iterator<Item = &'schema Node<Directive>>, FederationError> {
        Ok(self.get(&schema.schema)?.directives.iter())
    }

    pub(crate) fn get_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
        directive_name: &Name,
    ) -> Vec<&'schema Node<Directive>> {
        if let Some(field) = self.try_get(&schema.schema) {
            field
                .directives
                .iter()
                .filter(|directive| &directive.name == directive_name)
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Remove a directive application from this position by name.
    pub(crate) fn remove_directive_name(&self, schema: &mut FederationSchema, name: &str) {
        let Some(field) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        self.remove_directive_name_references(&mut schema.referencers, name);
        field
            .make_mut()
            .directives
            .retain(|other_directive| other_directive.name != name);
    }

    /// Remove a directive application.
    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Node<Directive>,
    ) {
        let Some(field) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        if !field.directives.iter().any(|other_directive| {
            (other_directive.name == directive.name) && !other_directive.ptr_eq(directive)
        }) {
            self.remove_directive_name_references(&mut schema.referencers, &directive.name);
        }
        field
            .make_mut()
            .directives
            .retain(|other_directive| !other_directive.ptr_eq(directive));
    }

    fn insert_references(
        &self,
        field: &Component<FieldDefinition>,
        referencers: &mut Referencers,
        allow_built_ins: bool,
    ) -> Result<(), FederationError> {
        if !allow_built_ins && is_graphql_reserved_name(&self.field_name) {
            bail!(r#"Cannot insert reserved object field "{self}""#);
        }
        validate_node_directives(field.directives.deref())?;
        for directive_reference in field.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        self.insert_type_references(field, referencers)?;
        validate_arguments(&field.arguments)?;
        for argument in field.arguments.iter() {
            self.argument(argument.name.clone())
                .insert_references(argument, referencers)?;
        }
        Ok(())
    }

    fn remove_references(
        &self,
        field: &Component<FieldDefinition>,
        referencers: &mut Referencers,
        allow_built_ins: bool,
    ) -> Result<(), FederationError> {
        if !allow_built_ins && is_graphql_reserved_name(&self.field_name) {
            bail!(r#"Cannot remove reserved object field "{self}""#);
        }
        for directive_reference in field.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
        self.remove_type_references(field, referencers);
        for argument in field.arguments.iter() {
            self.argument(argument.name.clone())
                .remove_references(argument, referencers)?;
        }
        Ok(())
    }

    fn insert_directive_name_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let directive_referencers = referencers.directives.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Object field \"{self}\"'s directive application \"@{name}\" does not refer to an existing directive.",
                ),
            }
        })?;
        directive_referencers.object_fields.insert(self.clone());
        Ok(())
    }

    fn remove_directive_name_references(&self, referencers: &mut Referencers, name: &str) {
        let Some(directive_referencers) = referencers.directives.get_mut(name) else {
            return;
        };
        directive_referencers.object_fields.shift_remove(self);
    }

    fn insert_type_references(
        &self,
        field: &Component<FieldDefinition>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let output_type_reference = field.ty.inner_named_type();
        if let Some(scalar_type_referencers) =
            referencers.scalar_types.get_mut(output_type_reference)
        {
            scalar_type_referencers.object_fields.insert(self.clone());
        } else if let Some(object_type_referencers) =
            referencers.object_types.get_mut(output_type_reference)
        {
            object_type_referencers.object_fields.insert(self.clone());
        } else if let Some(interface_type_referencers) =
            referencers.interface_types.get_mut(output_type_reference)
        {
            interface_type_referencers
                .object_fields
                .insert(self.clone());
        } else if let Some(union_type_referencers) =
            referencers.union_types.get_mut(output_type_reference)
        {
            union_type_referencers.object_fields.insert(self.clone());
        } else if let Some(enum_type_referencers) =
            referencers.enum_types.get_mut(output_type_reference)
        {
            enum_type_referencers.object_fields.insert(self.clone());
        } else {
            return Err(FederationError::internal(format!(
                "Object field \"{}\"'s inner type \"{}\" does not refer to an existing output type.",
                self,
                output_type_reference.deref(),
            )));
        }
        Ok(())
    }

    fn remove_type_references(
        &self,
        field: &Component<FieldDefinition>,
        referencers: &mut Referencers,
    ) {
        let output_type_reference = field.ty.inner_named_type();
        if let Some(scalar_type_referencers) =
            referencers.scalar_types.get_mut(output_type_reference)
        {
            scalar_type_referencers.object_fields.shift_remove(self);
        } else if let Some(object_type_referencers) =
            referencers.object_types.get_mut(output_type_reference)
        {
            object_type_referencers.object_fields.shift_remove(self);
        } else if let Some(interface_type_referencers) =
            referencers.interface_types.get_mut(output_type_reference)
        {
            interface_type_referencers.object_fields.shift_remove(self);
        } else if let Some(union_type_referencers) =
            referencers.union_types.get_mut(output_type_reference)
        {
            union_type_referencers.object_fields.shift_remove(self);
        } else if let Some(enum_type_referencers) =
            referencers.enum_types.get_mut(output_type_reference)
        {
            enum_type_referencers.object_fields.shift_remove(self);
        }
    }

    fn rename_type(
        &self,
        schema: &mut FederationSchema,
        new_name: Name,
    ) -> Result<(), FederationError> {
        let field = self.make_mut(&mut schema.schema)?.make_mut();
        rename_type(&mut field.ty, new_name);
        Ok(())
    }
}

impl Display for ObjectFieldDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.type_name, self.field_name)
    }
}

impl Debug for ObjectFieldDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "ObjectField({self})")
    }
}

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ObjectFieldArgumentDefinitionPosition {
    pub(crate) type_name: Name,
    pub(crate) field_name: Name,
    pub(crate) argument_name: Name,
}

impl ObjectFieldArgumentDefinitionPosition {
    pub(crate) fn get_all_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<impl Iterator<Item = &'schema Node<Directive>>, FederationError> {
        Ok(self.get(&schema.schema)?.directives.iter())
    }

    pub(crate) fn get_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
        directive_name: &Name,
    ) -> Vec<&'schema Node<Directive>> {
        if let Some(arg) = self.try_get(&schema.schema) {
            arg.directives
                .iter()
                .filter(|d| &d.name == directive_name)
                .collect()
        } else {
            Vec::new()
        }
    }
    pub(crate) fn parent(&self) -> ObjectFieldDefinitionPosition {
        ObjectFieldDefinitionPosition {
            type_name: self.type_name.clone(),
            field_name: self.field_name.clone(),
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<InputValueDefinition>, PositionLookupError> {
        let field = self.parent().get(schema)?;

        field.argument_by_name(&self.argument_name).ok_or_else(|| {
            PositionLookupError::MissingFieldArgument(
                "Object field",
                self.type_name.clone(),
                self.field_name.clone(),
                self.argument_name.clone(),
            )
        })
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema Node<InputValueDefinition>> {
        self.get(schema).ok()
    }

    fn make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Result<&'schema mut Node<InputValueDefinition>, PositionLookupError> {
        let parent = self.parent();
        let type_ = parent.make_mut(schema)?.make_mut();

        type_
            .arguments
            .iter_mut()
            .find(|a| a.name == self.argument_name)
            .ok_or_else(|| {
                PositionLookupError::MissingFieldArgument(
                    "Object field",
                    self.type_name.clone(),
                    self.field_name.clone(),
                    self.argument_name.clone(),
                )
            })
    }

    fn try_make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Option<&'schema mut Node<InputValueDefinition>> {
        if self.try_get(schema).is_some() {
            self.make_mut(schema).ok()
        } else {
            None
        }
    }

    /// Remove this argument from the field.
    ///
    /// This can make the schema invalid if this is an implementing field of an interface.
    pub(crate) fn remove(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let Some(argument) = self.try_get(&schema.schema) else {
            return Ok(());
        };
        self.remove_references(argument, &mut schema.referencers)?;
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .arguments
            .retain(|other_argument| other_argument.name != self.argument_name);
        Ok(())
    }

    pub(crate) fn insert_directive(
        &self,
        schema: &mut FederationSchema,
        directive: Node<Directive>,
    ) -> Result<(), FederationError> {
        let argument = self.make_mut(&mut schema.schema)?;
        if argument
            .directives
            .iter()
            .any(|other_directive| other_directive.ptr_eq(&directive))
        {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Directive application \"@{}\" already exists on object field argument \"{}\"",
                    directive.name, self,
                ),
            }
            .into());
        }
        let name = directive.name.clone();
        argument.make_mut().directives.push(directive);
        self.insert_directive_name_references(&mut schema.referencers, &name)
    }

    /// Remove a directive application from this position by name.
    pub(crate) fn remove_directive_name(&self, schema: &mut FederationSchema, name: &str) {
        let Some(argument) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        self.remove_directive_name_references(&mut schema.referencers, name);
        argument
            .make_mut()
            .directives
            .retain(|other_directive| other_directive.name != name);
    }

    fn insert_references(
        &self,
        argument: &Node<InputValueDefinition>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if is_graphql_reserved_name(&self.argument_name) {
            bail!(r#"Cannot insert reserved object field argument "{self}""#);
        }
        validate_node_directives(argument.directives.deref())?;
        for directive_reference in argument.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        self.insert_type_references(argument, referencers)
    }

    fn remove_references(
        &self,
        argument: &Node<InputValueDefinition>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if is_graphql_reserved_name(&self.argument_name) {
            bail!(r#"Cannot remove reserved object field argument "{self}""#);
        }
        for directive_reference in argument.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
        self.remove_type_references(argument, referencers);
        Ok(())
    }

    fn insert_directive_name_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let directive_referencers = referencers.directives.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Object field argument \"{self}\"'s directive application \"@{name}\" does not refer to an existing directive.",
                ),
            }
        })?;
        directive_referencers
            .object_field_arguments
            .insert(self.clone());
        Ok(())
    }

    fn remove_directive_name_references(&self, referencers: &mut Referencers, name: &str) {
        let Some(directive_referencers) = referencers.directives.get_mut(name) else {
            return;
        };
        directive_referencers
            .object_field_arguments
            .shift_remove(self);
    }

    fn insert_type_references(
        &self,
        argument: &Node<InputValueDefinition>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let input_type_reference = argument.ty.inner_named_type();
        if let Some(scalar_type_referencers) =
            referencers.scalar_types.get_mut(input_type_reference)
        {
            scalar_type_referencers
                .object_field_arguments
                .insert(self.clone());
        } else if let Some(enum_type_referencers) =
            referencers.enum_types.get_mut(input_type_reference)
        {
            enum_type_referencers
                .object_field_arguments
                .insert(self.clone());
        } else if let Some(input_object_type_referencers) =
            referencers.input_object_types.get_mut(input_type_reference)
        {
            input_object_type_referencers
                .object_field_arguments
                .insert(self.clone());
        } else {
            return Err(FederationError::internal(format!(
                "Object field argument \"{}\"'s inner type \"{}\" does not refer to an existing input type.",
                self,
                input_type_reference.deref(),
            )));
        }
        Ok(())
    }

    fn remove_type_references(
        &self,
        argument: &Node<InputValueDefinition>,
        referencers: &mut Referencers,
    ) {
        let input_type_reference = argument.ty.inner_named_type();
        if let Some(scalar_type_referencers) =
            referencers.scalar_types.get_mut(input_type_reference)
        {
            scalar_type_referencers
                .object_field_arguments
                .shift_remove(self);
        } else if let Some(enum_type_referencers) =
            referencers.enum_types.get_mut(input_type_reference)
        {
            enum_type_referencers
                .object_field_arguments
                .shift_remove(self);
        } else if let Some(input_object_type_referencers) =
            referencers.input_object_types.get_mut(input_type_reference)
        {
            input_object_type_referencers
                .object_field_arguments
                .shift_remove(self);
        }
    }

    fn rename_type(
        &self,
        schema: &mut FederationSchema,
        new_name: Name,
    ) -> Result<(), FederationError> {
        let field = self.make_mut(&mut schema.schema)?.make_mut();
        rename_type(field.ty.make_mut(), new_name);
        Ok(())
    }
}

impl Display for ObjectFieldArgumentDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}.{}({}:)",
            self.type_name, self.field_name, self.argument_name
        )
    }
}

impl Debug for ObjectFieldArgumentDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "ObjectFieldArgument({self})")
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct ObjectOrInterfaceFieldDirectivePosition {
    pub(crate) field: ObjectOrInterfaceFieldDefinitionPosition,
    pub(crate) directive_name: Name,
    pub(crate) directive_index: usize,
}

impl ObjectOrInterfaceFieldDirectivePosition {
    // NOTE: this is used only for connectors "expansion" code and can be
    // deleted after connectors switches to use the composition port
    pub(crate) fn add_argument(
        &self,
        schema: &mut FederationSchema,
        argument: Node<Argument>,
    ) -> Result<(), FederationError> {
        let directive = match self.field {
            ObjectOrInterfaceFieldDefinitionPosition::Object(ref field) => {
                let field = field.make_mut(&mut schema.schema)?;

                field
                    .make_mut()
                    .directives
                    .get_mut(self.directive_index)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Object field \"{}\"'s directive application at index {} does not exist",
                            self.field, self.directive_index,
                        ),
                    })?
            }
            ObjectOrInterfaceFieldDefinitionPosition::Interface(ref field) => {
                let field = field.make_mut(&mut schema.schema)?;

                field
                    .make_mut()
                    .directives
                    .get_mut(self.directive_index)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Interface field \"{}\"'s directive application at index {} does not exist",
                            self.field, self.directive_index,
                        ),
                    })?
            }
        };

        directive.make_mut().arguments.push(argument);

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub(crate) struct InterfaceTypeDefinitionPosition {
    pub(crate) type_name: Name,
}

impl InterfaceTypeDefinitionPosition {
    const EXPECTED: &'static str = "an interface type";

    pub(crate) fn new(type_name: Name) -> Self {
        Self { type_name }
    }

    pub(crate) fn field(&self, field_name: Name) -> InterfaceFieldDefinitionPosition {
        InterfaceFieldDefinitionPosition {
            type_name: self.type_name.clone(),
            field_name,
        }
    }

    pub(crate) fn introspection_typename_field(&self) -> InterfaceFieldDefinitionPosition {
        self.field(INTROSPECTION_TYPENAME_FIELD_NAME.clone())
    }

    pub(crate) fn fields<'a>(
        &'a self,
        schema: &'a Schema,
    ) -> Result<impl Iterator<Item = InterfaceFieldDefinitionPosition>, FederationError> {
        Ok(self
            .get(schema)?
            .fields
            .keys()
            .map(|name| self.field(name.clone())))
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<InterfaceType>, PositionLookupError> {
        schema
            .types
            .get(&self.type_name)
            .ok_or_else(|| PositionLookupError::TypeMissing(self.type_name.clone()))
            .and_then(|type_| {
                if let ExtendedType::Interface(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(PositionLookupError::TypeWrongKind(
                        self.type_name.clone(),
                        Self::EXPECTED,
                    ))
                }
            })
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema Node<InterfaceType>> {
        self.get(schema).ok()
    }

    fn make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Result<&'schema mut Node<InterfaceType>, PositionLookupError> {
        schema
            .types
            .get_mut(&self.type_name)
            .ok_or_else(|| PositionLookupError::TypeMissing(self.type_name.clone()))
            .and_then(|type_| {
                if let ExtendedType::Interface(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(PositionLookupError::TypeWrongKind(
                        self.type_name.clone(),
                        Self::EXPECTED,
                    ))
                }
            })
    }

    fn try_make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Option<&'schema mut Node<InterfaceType>> {
        if self.try_get(schema).is_some() {
            self.make_mut(schema).ok()
        } else {
            None
        }
    }

    pub(crate) fn pre_insert(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        if schema.referencers.contains_type_name(&self.type_name) {
            bail!(r#"Type "{self}" has already been pre-inserted"#);
        }
        schema
            .referencers
            .interface_types
            .insert(self.type_name.clone(), Default::default());
        Ok(())
    }

    pub(crate) fn insert(
        &self,
        schema: &mut FederationSchema,
        type_: Node<InterfaceType>,
    ) -> Result<(), FederationError> {
        if self.type_name != type_.name {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Interface type \"{}\" given type named \"{}\"",
                    self, type_.name,
                ),
            }
            .into());
        }
        if !schema
            .referencers
            .interface_types
            .contains_key(&self.type_name)
        {
            bail!(r#"Type "{self}" has not been pre-inserted"#);
        }
        if schema.schema.types.contains_key(&self.type_name) {
            bail!(r#"Type "{self}" already exists in schema"#);
        }
        schema
            .schema
            .types
            .insert(self.type_name.clone(), ExtendedType::Interface(type_));
        self.insert_references(
            self.get(&schema.schema)?,
            &schema.schema,
            &mut schema.referencers,
        )
    }

    pub(crate) fn insert_empty(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        self.insert(
            schema,
            Node::new(InterfaceType {
                description: None,
                name: self.type_name.clone(),
                implements_interfaces: Default::default(),
                fields: Default::default(),
                directives: Default::default(),
            }),
        )
    }

    /// Remove this interface from the schema, and any direct references to the interface.
    ///
    /// This can make the schema invalid if this interface is referenced by a field that is the only
    /// field of its type. Removing that reference will make its parent type empty.
    pub(crate) fn remove(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<Option<InterfaceTypeReferencers>, FederationError> {
        let Some(referencers) = self.remove_internal(schema)? else {
            return Ok(None);
        };
        for type_ in &referencers.object_types {
            type_.remove_implements_interface(schema, &self.type_name);
        }
        for field in &referencers.object_fields {
            field.remove(schema)?;
        }
        for type_ in &referencers.interface_types {
            type_.remove_implements_interface(schema, &self.type_name);
        }
        for field in &referencers.interface_fields {
            field.remove(schema)?;
        }
        Ok(Some(referencers))
    }

    /// Remove this interface from the schema, and recursively remove references to the interface.
    pub(crate) fn remove_recursive(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        let Some(referencers) = self.remove_internal(schema)? else {
            return Ok(());
        };
        for type_ in referencers.object_types {
            type_.remove_implements_interface(schema, &self.type_name);
        }
        for field in referencers.object_fields {
            field.remove_recursive(schema)?;
        }
        for type_ in referencers.interface_types {
            type_.remove_implements_interface(schema, &self.type_name);
        }
        for field in referencers.interface_fields {
            field.remove_recursive(schema)?;
        }
        Ok(())
    }

    fn remove_internal(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<Option<InterfaceTypeReferencers>, FederationError> {
        let Some(type_) = self.try_get(&schema.schema) else {
            return Ok(None);
        };
        self.remove_references(type_, &schema.schema, &mut schema.referencers)?;
        schema.schema.types.shift_remove(&self.type_name);
        Ok(Some(
            schema
                .referencers
                .interface_types
                .shift_remove(&self.type_name)
                .ok_or_else(|| SingleFederationError::Internal {
                    message: format!("Schema missing referencers for type \"{self}\""),
                })?,
        ))
    }

    pub(crate) fn insert_directive(
        &self,
        schema: &mut FederationSchema,
        directive: Component<Directive>,
    ) -> Result<(), FederationError> {
        let type_ = self.make_mut(&mut schema.schema)?;
        if type_
            .directives
            .iter()
            .any(|other_directive| other_directive.ptr_eq(&directive))
        {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Directive application \"@{}\" already exists on interface type \"{}\"",
                    directive.name, self,
                ),
            }
            .into());
        }
        let name = directive.name.clone();
        type_.make_mut().directives.push(directive);
        self.insert_directive_name_references(&mut schema.referencers, &name)
    }

    /// Remove a directive application from this position by name.
    pub(crate) fn remove_directive_name(&self, schema: &mut FederationSchema, name: &str) {
        let Some(type_) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        self.remove_directive_name_references(&mut schema.referencers, name);
        type_
            .make_mut()
            .directives
            .retain(|other_directive| other_directive.name != name);
    }

    pub(crate) fn insert_implements_interface(
        &self,
        schema: &mut FederationSchema,
        name: ComponentName,
    ) -> Result<(), FederationError> {
        let type_ = self.make_mut(&mut schema.schema)?;
        type_.make_mut().implements_interfaces.insert(name.clone());
        self.insert_implements_interface_references(&mut schema.referencers, &name)
    }

    pub(crate) fn remove_implements_interface(&self, schema: &mut FederationSchema, name: &str) {
        let Some(type_) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        self.remove_implements_interface_references(&mut schema.referencers, name);
        type_
            .make_mut()
            .implements_interfaces
            .retain(|other_type| other_type != name);
    }

    fn insert_references(
        &self,
        type_: &Node<InterfaceType>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        validate_component_directives(type_.directives.deref())?;
        for directive_reference in type_.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        for interface_type_reference in type_.implements_interfaces.iter() {
            self.insert_implements_interface_references(
                referencers,
                interface_type_reference.deref(),
            )?;
        }
        let introspection_typename_field = self.introspection_typename_field();
        introspection_typename_field.insert_references(
            introspection_typename_field.get(schema)?,
            referencers,
            true,
        )?;
        for (field_name, field) in type_.fields.iter() {
            self.field(field_name.clone())
                .insert_references(field, referencers, false)?;
        }
        Ok(())
    }

    fn remove_references(
        &self,
        type_: &Node<InterfaceType>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        for directive_reference in type_.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
        for interface_type_reference in type_.implements_interfaces.iter() {
            self.remove_implements_interface_references(
                referencers,
                interface_type_reference.deref(),
            );
        }
        let introspection_typename_field = self.introspection_typename_field();
        introspection_typename_field.remove_references(
            introspection_typename_field.get(schema)?,
            referencers,
            true,
        )?;
        for (field_name, field) in type_.fields.iter() {
            self.field(field_name.clone())
                .remove_references(field, referencers, false)?;
        }
        Ok(())
    }

    fn insert_directive_name_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let directive_referencers = referencers.directives.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Interface type \"{self}\"'s directive application \"@{name}\" does not refer to an existing directive.",
                ),
            }
        })?;
        directive_referencers.interface_types.insert(self.clone());
        Ok(())
    }

    fn remove_directive_name_references(&self, referencers: &mut Referencers, name: &str) {
        let Some(directive_referencers) = referencers.directives.get_mut(name) else {
            return;
        };
        directive_referencers.interface_types.shift_remove(self);
    }

    fn insert_implements_interface_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let interface_type_referencers = referencers.interface_types.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Interface type \"{self}\"'s implements \"{name}\" does not refer to an existing interface.",
                ),
            }
        })?;
        interface_type_referencers
            .interface_types
            .insert(self.clone());
        Ok(())
    }

    fn remove_implements_interface_references(&self, referencers: &mut Referencers, name: &str) {
        let Some(interface_type_referencers) = referencers.interface_types.get_mut(name) else {
            return;
        };
        interface_type_referencers
            .interface_types
            .shift_remove(self);
    }

    fn rename(&self, schema: &mut FederationSchema, new_name: Name) -> Result<(), FederationError> {
        self.make_mut(&mut schema.schema)?.make_mut().name = new_name.clone();

        if let Some(interface_type_referencers) = schema
            .referencers
            .interface_types
            .swap_remove(&self.type_name)
        {
            for pos in interface_type_referencers.object_types.iter() {
                pos.rename_implemented_interface(schema, &self.type_name, new_name.clone())?;
            }
            for pos in interface_type_referencers.object_fields.iter() {
                pos.rename_type(schema, new_name.clone())?;
            }
            for pos in interface_type_referencers.interface_types.iter() {
                pos.rename_implemented_interface(schema, &self.type_name, new_name.clone())?;
            }
            for pos in interface_type_referencers.interface_fields.iter() {
                pos.rename_type(schema, new_name.clone())?;
            }
            schema
                .referencers
                .interface_types
                .insert(new_name, interface_type_referencers);
        }

        Ok(())
    }

    fn rename_implemented_interface(
        &self,
        schema: &mut FederationSchema,
        old_name: &Name,
        new_name: Name,
    ) -> Result<(), FederationError> {
        let type_ = self.make_mut(&mut schema.schema)?.make_mut();
        type_.implements_interfaces.swap_remove(old_name);
        type_.implements_interfaces.insert(new_name.into());
        Ok(())
    }

    fn remove_extensions(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let type_ = self.make_mut(&mut schema.schema)?.make_mut();
        for directive in type_.directives.iter_mut() {
            directive.origin = ComponentOrigin::Definition;
        }
        type_.implements_interfaces = type_
            .implements_interfaces
            .iter()
            .map(|i| {
                let mut i = i.clone();
                i.origin = ComponentOrigin::Definition;
                i
            })
            .collect();
        for (_, field) in type_.fields.iter_mut() {
            field.origin = ComponentOrigin::Definition;
        }
        Ok(())
    }

    pub(crate) fn has_applied_directive(
        &self,
        schema: &FederationSchema,
        directive_name: &Name,
    ) -> bool {
        if let Some(type_) = self.try_get(schema.schema()) {
            return type_
                .directives
                .iter()
                .any(|directive| &directive.name == directive_name);
        }
        false
    }

    pub(crate) fn get_all_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<impl Iterator<Item = &'schema Component<Directive>>, FederationError> {
        Ok(self.get(&schema.schema)?.directives.iter())
    }

    pub(crate) fn get_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
        directive_name: &Name,
    ) -> Vec<&'schema Component<Directive>> {
        if let Some(field) = self.try_get(&schema.schema) {
            field
                .directives
                .iter()
                .filter(|directive| &directive.name == directive_name)
                .collect()
        } else {
            Vec::new()
        }
    }

    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Component<Directive>,
    ) {
        let Some(obj) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        if !obj.directives.iter().any(|other_directive| {
            (other_directive.name == directive.name) && !other_directive.ptr_eq(directive)
        }) {
            self.remove_directive_name_references(&mut schema.referencers, &directive.name);
        }
        obj.make_mut()
            .directives
            .retain(|other_directive| !other_directive.ptr_eq(directive));
    }
}

impl Display for InterfaceTypeDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.type_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub(crate) struct InterfaceFieldDefinitionPosition {
    pub(crate) type_name: Name,
    pub(crate) field_name: Name,
}

impl InterfaceFieldDefinitionPosition {
    pub(crate) fn parent(&self) -> InterfaceTypeDefinitionPosition {
        InterfaceTypeDefinitionPosition {
            type_name: self.type_name.clone(),
        }
    }

    pub(crate) fn argument(&self, argument_name: Name) -> InterfaceFieldArgumentDefinitionPosition {
        InterfaceFieldArgumentDefinitionPosition {
            type_name: self.type_name.clone(),
            field_name: self.field_name.clone(),
            argument_name,
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Component<FieldDefinition>, PositionLookupError> {
        let parent = self.parent();
        parent.get(schema)?;

        schema
            .type_field(&self.type_name, &self.field_name)
            .map_err(|_| {
                PositionLookupError::MissingField(
                    "Interface",
                    self.type_name.clone(),
                    self.field_name.clone(),
                )
            })
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema Component<FieldDefinition>> {
        self.get(schema).ok()
    }

    fn make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Result<&'schema mut Component<FieldDefinition>, PositionLookupError> {
        let parent = self.parent();
        let type_ = parent.make_mut(schema)?.make_mut();

        if is_graphql_reserved_name(&self.field_name) {
            return Err(PositionLookupError::MutateReservedField(
                "interface field",
                self.type_name.clone(),
                self.field_name.clone(),
            ));
        }
        type_.fields.get_mut(&self.field_name).ok_or_else(|| {
            PositionLookupError::MissingField(
                "Interface",
                self.type_name.clone(),
                self.field_name.clone(),
            )
        })
    }

    fn try_make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Option<&'schema mut Component<FieldDefinition>> {
        if self.try_get(schema).is_some() {
            self.make_mut(schema).ok()
        } else {
            None
        }
    }

    pub(crate) fn insert(
        &self,
        schema: &mut FederationSchema,
        field: Component<FieldDefinition>,
    ) -> Result<(), FederationError> {
        if self.field_name != field.name {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Interface field \"{}\" given field named \"{}\"",
                    self, field.name,
                ),
            }
            .into());
        }
        if self.try_get(&schema.schema).is_some() {
            bail!(r#"Interface field "{self}" already exists in schema"#);
        }
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .fields
            .insert(self.field_name.clone(), field);
        self.insert_references(self.get(&schema.schema)?, &mut schema.referencers, false)
    }

    /// Remove this field from its interface.
    ///
    /// This may make the schema invalid if the field is required by a parent interface, or if the
    /// field is the only field on its interface.
    pub(crate) fn remove(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let Some(field) = self.try_get(&schema.schema) else {
            return Ok(());
        };
        self.remove_references(field, &mut schema.referencers, false)?;
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .fields
            .shift_remove(&self.field_name);
        Ok(())
    }

    /// Remove this field from its interface. If this is the only field on its interface, remove
    /// the interface as well.
    ///
    /// This may make the schema invalid if the field is required by a parent interface.
    pub(crate) fn remove_recursive(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        self.remove(schema)?;
        let parent = self.parent();
        let Some(type_) = parent.try_get(&schema.schema) else {
            return Ok(());
        };
        if type_.fields.is_empty() {
            parent.remove_recursive(schema)?;
        }
        Ok(())
    }

    pub(crate) fn insert_directive(
        &self,
        schema: &mut FederationSchema,
        directive: Node<Directive>,
    ) -> Result<(), FederationError> {
        let field = self.make_mut(&mut schema.schema)?;
        if field
            .directives
            .iter()
            .any(|other_directive| other_directive.ptr_eq(&directive))
        {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Directive application \"@{}\" already exists on interface field \"{}\"",
                    directive.name, self,
                ),
            }
            .into());
        }
        let name = directive.name.clone();
        field.make_mut().directives.push(directive);
        self.insert_directive_name_references(&mut schema.referencers, &name)
    }

    pub(crate) fn get_all_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<impl Iterator<Item = &'schema Node<Directive>>, FederationError> {
        Ok(self.get(&schema.schema)?.directives.iter())
    }

    pub(crate) fn get_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
        directive_name: &Name,
    ) -> Vec<&'schema Node<Directive>> {
        if let Some(field) = self.try_get(&schema.schema) {
            field
                .directives
                .iter()
                .filter(|directive| &directive.name == directive_name)
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Remove a directive application from this position by name.
    pub(crate) fn remove_directive_name(&self, schema: &mut FederationSchema, name: &str) {
        let Some(field) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        self.remove_directive_name_references(&mut schema.referencers, name);
        field
            .make_mut()
            .directives
            .retain(|other_directive| other_directive.name != name);
    }

    /// Remove a directive application.
    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Node<Directive>,
    ) {
        let Some(field) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        if !field.directives.iter().any(|other_directive| {
            (other_directive.name == directive.name) && !other_directive.ptr_eq(directive)
        }) {
            self.remove_directive_name_references(&mut schema.referencers, &directive.name);
        }
        field
            .make_mut()
            .directives
            .retain(|other_directive| !other_directive.ptr_eq(directive));
    }

    fn insert_references(
        &self,
        field: &Component<FieldDefinition>,
        referencers: &mut Referencers,
        allow_built_ins: bool,
    ) -> Result<(), FederationError> {
        if !allow_built_ins && is_graphql_reserved_name(&self.field_name) {
            bail!(r#"Cannot insert reserved interface field "{self}""#);
        }
        validate_node_directives(field.directives.deref())?;
        for directive_reference in field.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        self.insert_type_references(field, referencers)?;
        validate_arguments(&field.arguments)?;
        for argument in field.arguments.iter() {
            self.argument(argument.name.clone())
                .insert_references(argument, referencers)?;
        }
        Ok(())
    }

    fn remove_references(
        &self,
        field: &Component<FieldDefinition>,
        referencers: &mut Referencers,
        allow_built_ins: bool,
    ) -> Result<(), FederationError> {
        if !allow_built_ins && is_graphql_reserved_name(&self.field_name) {
            bail!(r#"Cannot remove reserved interface field "{self}""#);
        }
        for directive_reference in field.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
        self.remove_type_references(field, referencers);
        for argument in field.arguments.iter() {
            self.argument(argument.name.clone())
                .remove_references(argument, referencers)?;
        }
        Ok(())
    }

    fn insert_directive_name_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let directive_referencers = referencers.directives.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Interface field \"{self}\"'s directive application \"@{name}\" does not refer to an existing directive.",
                ),
            }
        })?;
        directive_referencers.interface_fields.insert(self.clone());
        Ok(())
    }

    fn remove_directive_name_references(&self, referencers: &mut Referencers, name: &str) {
        let Some(directive_referencers) = referencers.directives.get_mut(name) else {
            return;
        };
        directive_referencers.interface_fields.shift_remove(self);
    }

    fn insert_type_references(
        &self,
        field: &Component<FieldDefinition>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let output_type_reference = field.ty.inner_named_type();
        if let Some(scalar_type_referencers) =
            referencers.scalar_types.get_mut(output_type_reference)
        {
            scalar_type_referencers
                .interface_fields
                .insert(self.clone());
        } else if let Some(object_type_referencers) =
            referencers.object_types.get_mut(output_type_reference)
        {
            object_type_referencers
                .interface_fields
                .insert(self.clone());
        } else if let Some(interface_type_referencers) =
            referencers.interface_types.get_mut(output_type_reference)
        {
            interface_type_referencers
                .interface_fields
                .insert(self.clone());
        } else if let Some(union_type_referencers) =
            referencers.union_types.get_mut(output_type_reference)
        {
            union_type_referencers.interface_fields.insert(self.clone());
        } else if let Some(enum_type_referencers) =
            referencers.enum_types.get_mut(output_type_reference)
        {
            enum_type_referencers.interface_fields.insert(self.clone());
        } else {
            return Err(FederationError::internal(format!(
                "Interface field \"{}\"'s inner type \"{}\" does not refer to an existing output type.",
                self,
                output_type_reference.deref(),
            )));
        }
        Ok(())
    }

    fn remove_type_references(
        &self,
        field: &Component<FieldDefinition>,
        referencers: &mut Referencers,
    ) {
        let output_type_reference = field.ty.inner_named_type();
        if let Some(scalar_type_referencers) =
            referencers.scalar_types.get_mut(output_type_reference)
        {
            scalar_type_referencers.interface_fields.shift_remove(self);
        } else if let Some(object_type_referencers) =
            referencers.object_types.get_mut(output_type_reference)
        {
            object_type_referencers.interface_fields.shift_remove(self);
        } else if let Some(interface_type_referencers) =
            referencers.interface_types.get_mut(output_type_reference)
        {
            interface_type_referencers
                .interface_fields
                .shift_remove(self);
        } else if let Some(union_type_referencers) =
            referencers.union_types.get_mut(output_type_reference)
        {
            union_type_referencers.interface_fields.shift_remove(self);
        } else if let Some(enum_type_referencers) =
            referencers.enum_types.get_mut(output_type_reference)
        {
            enum_type_referencers.interface_fields.shift_remove(self);
        }
    }

    fn rename_type(
        &self,
        schema: &mut FederationSchema,
        new_name: Name,
    ) -> Result<(), FederationError> {
        let field = self.make_mut(&mut schema.schema)?.make_mut();
        match field.ty.clone() {
            ast::Type::Named(_) => field.ty = ast::Type::Named(new_name),
            ast::Type::NonNullNamed(_) => field.ty = ast::Type::NonNullNamed(new_name),
            ast::Type::List(_) => todo!(),
            ast::Type::NonNullList(_) => todo!(),
        }
        Ok(())
    }
}

impl Display for InterfaceFieldDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.type_name, self.field_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct InterfaceFieldArgumentDefinitionPosition {
    pub(crate) type_name: Name,
    pub(crate) field_name: Name,
    pub(crate) argument_name: Name,
}

impl InterfaceFieldArgumentDefinitionPosition {
    pub(crate) fn get_all_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<impl Iterator<Item = &'schema Node<Directive>>, FederationError> {
        Ok(self.get(&schema.schema)?.directives.iter())
    }

    pub(crate) fn get_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
        directive_name: &Name,
    ) -> Vec<&'schema Node<Directive>> {
        if let Some(arg) = self.try_get(&schema.schema) {
            arg.directives
                .iter()
                .filter(|d| &d.name == directive_name)
                .collect()
        } else {
            Vec::new()
        }
    }
    pub(crate) fn parent(&self) -> InterfaceFieldDefinitionPosition {
        InterfaceFieldDefinitionPosition {
            type_name: self.type_name.clone(),
            field_name: self.field_name.clone(),
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<InputValueDefinition>, PositionLookupError> {
        let field = self.parent().get(schema)?;

        field.argument_by_name(&self.argument_name).ok_or_else(|| {
            PositionLookupError::MissingFieldArgument(
                "Interface field",
                self.type_name.clone(),
                self.field_name.clone(),
                self.argument_name.clone(),
            )
        })
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema Node<InputValueDefinition>> {
        self.get(schema).ok()
    }

    fn make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Result<&'schema mut Node<InputValueDefinition>, PositionLookupError> {
        let parent = self.parent();
        let type_ = parent.make_mut(schema)?.make_mut();

        type_
            .arguments
            .iter_mut()
            .find(|a| a.name == self.argument_name)
            .ok_or_else(|| {
                PositionLookupError::MissingFieldArgument(
                    "Interface field",
                    self.type_name.clone(),
                    self.field_name.clone(),
                    self.argument_name.clone(),
                )
            })
    }

    fn try_make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Option<&'schema mut Node<InputValueDefinition>> {
        if self.try_get(schema).is_some() {
            self.make_mut(schema).ok()
        } else {
            None
        }
    }

    /// Remove this argument from its field definition.
    ///
    /// This can make the schema invalid if this argument is required and also declared in
    /// implementers of this interface.
    pub(crate) fn remove(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let Some(argument) = self.try_get(&schema.schema) else {
            return Ok(());
        };
        self.remove_references(argument, &mut schema.referencers)?;
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .arguments
            .retain(|other_argument| other_argument.name != self.argument_name);
        Ok(())
    }

    pub(crate) fn insert_directive(
        &self,
        schema: &mut FederationSchema,
        directive: Node<Directive>,
    ) -> Result<(), FederationError> {
        let argument = self.make_mut(&mut schema.schema)?;
        if argument
            .directives
            .iter()
            .any(|other_directive| other_directive.ptr_eq(&directive))
        {
            return Err(
                SingleFederationError::Internal {
                    message: format!(
                        "Directive application \"@{}\" already exists on interface field argument \"{}\"",
                        directive.name,
                        self,
                    )
                }.into()
            );
        }
        let name = directive.name.clone();
        argument.make_mut().directives.push(directive);
        self.insert_directive_name_references(&mut schema.referencers, &name)
    }

    /// Remove a directive application from this position by name.
    pub(crate) fn remove_directive_name(&self, schema: &mut FederationSchema, name: &str) {
        let Some(argument) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        self.remove_directive_name_references(&mut schema.referencers, name);
        argument
            .make_mut()
            .directives
            .retain(|other_directive| other_directive.name != name);
    }

    fn insert_references(
        &self,
        argument: &Node<InputValueDefinition>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if is_graphql_reserved_name(&self.argument_name) {
            bail!(r#"Cannot insert reserved interface field argument "{self}""#);
        }
        validate_node_directives(argument.directives.deref())?;
        for directive_reference in argument.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        self.insert_type_references(argument, referencers)
    }

    fn remove_references(
        &self,
        argument: &Node<InputValueDefinition>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if is_graphql_reserved_name(&self.argument_name) {
            bail!(r#"Cannot remove reserved interface field argument "{self}""#);
        }
        for directive_reference in argument.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
        self.remove_type_references(argument, referencers);
        Ok(())
    }

    fn insert_directive_name_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let directive_referencers = referencers.directives.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Interface field argument \"{self}\"'s directive application \"@{name}\" does not refer to an existing directive.",
                ),
            }
        })?;
        directive_referencers
            .interface_field_arguments
            .insert(self.clone());
        Ok(())
    }

    fn remove_directive_name_references(&self, referencers: &mut Referencers, name: &str) {
        let Some(directive_referencers) = referencers.directives.get_mut(name) else {
            return;
        };
        directive_referencers
            .interface_field_arguments
            .shift_remove(self);
    }

    fn insert_type_references(
        &self,
        argument: &Node<InputValueDefinition>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let input_type_reference = argument.ty.inner_named_type();
        if let Some(scalar_type_referencers) =
            referencers.scalar_types.get_mut(input_type_reference)
        {
            scalar_type_referencers
                .interface_field_arguments
                .insert(self.clone());
        } else if let Some(enum_type_referencers) =
            referencers.enum_types.get_mut(input_type_reference)
        {
            enum_type_referencers
                .interface_field_arguments
                .insert(self.clone());
        } else if let Some(input_object_type_referencers) =
            referencers.input_object_types.get_mut(input_type_reference)
        {
            input_object_type_referencers
                .interface_field_arguments
                .insert(self.clone());
        } else {
            return Err(FederationError::internal(format!(
                "Interface field argument \"{}\"'s inner type \"{}\" does not refer to an existing input type.",
                self,
                input_type_reference.deref(),
            )));
        }
        Ok(())
    }

    fn remove_type_references(
        &self,
        argument: &Node<InputValueDefinition>,
        referencers: &mut Referencers,
    ) {
        let input_type_reference = argument.ty.inner_named_type();
        if let Some(scalar_type_referencers) =
            referencers.scalar_types.get_mut(input_type_reference)
        {
            scalar_type_referencers
                .interface_field_arguments
                .shift_remove(self);
        } else if let Some(enum_type_referencers) =
            referencers.enum_types.get_mut(input_type_reference)
        {
            enum_type_referencers
                .interface_field_arguments
                .shift_remove(self);
        } else if let Some(input_object_type_referencers) =
            referencers.input_object_types.get_mut(input_type_reference)
        {
            input_object_type_referencers
                .interface_field_arguments
                .shift_remove(self);
        }
    }

    fn rename_type(
        &self,
        schema: &mut FederationSchema,
        new_name: Name,
    ) -> Result<(), FederationError> {
        let argument = self.make_mut(&mut schema.schema)?.make_mut();
        match argument.ty.as_ref() {
            ast::Type::Named(_) => *argument.ty.make_mut() = ast::Type::Named(new_name),
            ast::Type::NonNullNamed(_) => {
                *argument.ty.make_mut() = ast::Type::NonNullNamed(new_name)
            }
            ast::Type::List(_) => todo!(),
            ast::Type::NonNullList(_) => todo!(),
        }
        Ok(())
    }
}

impl Display for InterfaceFieldArgumentDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}.{}({}:)",
            self.type_name, self.field_name, self.argument_name
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub(crate) struct UnionTypeDefinitionPosition {
    pub(crate) type_name: Name,
}

impl UnionTypeDefinitionPosition {
    const EXPECTED: &'static str = "a union type";

    pub(crate) fn introspection_typename_field(&self) -> UnionTypenameFieldDefinitionPosition {
        UnionTypenameFieldDefinitionPosition {
            type_name: self.type_name.clone(),
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<UnionType>, PositionLookupError> {
        schema
            .types
            .get(&self.type_name)
            .ok_or_else(|| PositionLookupError::TypeMissing(self.type_name.clone()))
            .and_then(|type_| {
                if let ExtendedType::Union(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(PositionLookupError::TypeWrongKind(
                        self.type_name.clone(),
                        Self::EXPECTED,
                    ))
                }
            })
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema Node<UnionType>> {
        self.get(schema).ok()
    }

    fn make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Result<&'schema mut Node<UnionType>, PositionLookupError> {
        schema
            .types
            .get_mut(&self.type_name)
            .ok_or_else(|| PositionLookupError::TypeMissing(self.type_name.clone()))
            .and_then(|type_| {
                if let ExtendedType::Union(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(PositionLookupError::TypeWrongKind(
                        self.type_name.clone(),
                        Self::EXPECTED,
                    ))
                }
            })
    }

    fn try_make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Option<&'schema mut Node<UnionType>> {
        if self.try_get(schema).is_some() {
            self.make_mut(schema).ok()
        } else {
            None
        }
    }

    pub(crate) fn pre_insert(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        if schema.referencers.contains_type_name(&self.type_name) {
            bail!(r#"Type "{self}" has already been pre-inserted"#);
        }
        schema
            .referencers
            .union_types
            .insert(self.type_name.clone(), Default::default());
        Ok(())
    }

    pub(crate) fn insert(
        &self,
        schema: &mut FederationSchema,
        type_: Node<UnionType>,
    ) -> Result<(), FederationError> {
        if self.type_name != type_.name {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Union type \"{}\" given type named \"{}\"",
                    self, type_.name,
                ),
            }
            .into());
        }
        if !schema.referencers.union_types.contains_key(&self.type_name) {
            bail!(r#"Type "{self}" has not been pre-inserted"#);
        }
        if schema.schema.types.contains_key(&self.type_name) {
            bail!(r#"Type "{self}" already exists in schema"#);
        }
        schema
            .schema
            .types
            .insert(self.type_name.clone(), ExtendedType::Union(type_));
        self.insert_references(self.get(&schema.schema)?, &mut schema.referencers)
    }

    /// Remove this union from the schema, and remove any direct references to the union.
    ///
    /// This can make the schema invalid if the fields referencing the union are the only fields of
    /// their type. That would cause the type definition to become empty.
    pub(crate) fn remove(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<Option<UnionTypeReferencers>, FederationError> {
        let Some(referencers) = self.remove_internal(schema)? else {
            return Ok(None);
        };
        for field in &referencers.object_fields {
            field.remove(schema)?;
        }
        for field in &referencers.interface_fields {
            field.remove(schema)?;
        }
        Ok(Some(referencers))
    }

    /// Remove this union from the schema, and recursively remove references to the union.
    pub(crate) fn remove_recursive(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        let Some(referencers) = self.remove_internal(schema)? else {
            return Ok(());
        };
        for field in referencers.object_fields {
            field.remove_recursive(schema)?;
        }
        for field in referencers.interface_fields {
            field.remove_recursive(schema)?;
        }
        Ok(())
    }

    fn remove_internal(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<Option<UnionTypeReferencers>, FederationError> {
        let Some(type_) = self.try_get(&schema.schema) else {
            return Ok(None);
        };
        self.remove_references(type_, &mut schema.referencers);
        schema.schema.types.shift_remove(&self.type_name);
        Ok(Some(
            schema
                .referencers
                .union_types
                .shift_remove(&self.type_name)
                .ok_or_else(|| SingleFederationError::Internal {
                    message: format!("Schema missing referencers for type \"{self}\""),
                })?,
        ))
    }

    pub(crate) fn insert_directive(
        &self,
        schema: &mut FederationSchema,
        directive: Component<Directive>,
    ) -> Result<(), FederationError> {
        let type_ = self.make_mut(&mut schema.schema)?;
        if type_
            .directives
            .iter()
            .any(|other_directive| other_directive.ptr_eq(&directive))
        {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Directive application \"@{}\" already exists on union type \"{}\"",
                    directive.name, self,
                ),
            }
            .into());
        }
        let name = directive.name.clone();
        type_.make_mut().directives.push(directive);
        self.insert_directive_name_references(&mut schema.referencers, &name)
    }

    /// Remove a directive application from this position by name.
    pub(crate) fn remove_directive_name(&self, schema: &mut FederationSchema, name: &str) {
        let Some(type_) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        self.remove_directive_name_references(&mut schema.referencers, name);
        type_
            .make_mut()
            .directives
            .retain(|other_directive| other_directive.name != name);
    }

    pub(crate) fn insert_member(
        &self,
        schema: &mut FederationSchema,
        name: ComponentName,
    ) -> Result<(), FederationError> {
        let type_ = self.make_mut(&mut schema.schema)?;
        type_.make_mut().members.insert(name.clone());
        self.insert_member_references(&mut schema.referencers, &name)
    }

    pub(crate) fn remove_member(&self, schema: &mut FederationSchema, name: &str) {
        let Some(type_) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        self.remove_member_references(&mut schema.referencers, name);
        type_
            .make_mut()
            .members
            .retain(|other_type| other_type != name);
    }

    pub(crate) fn remove_member_recursive(
        &self,
        schema: &mut FederationSchema,
        name: &str,
    ) -> Result<(), FederationError> {
        self.remove_member(schema, name);
        let Some(type_) = self.try_get(&schema.schema) else {
            return Ok(());
        };
        if type_.members.is_empty() {
            self.remove_recursive(schema)?;
        }
        Ok(())
    }

    fn insert_references(
        &self,
        type_: &Node<UnionType>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        validate_component_directives(type_.directives.deref())?;
        for directive_reference in type_.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        for object_type_reference in type_.members.iter() {
            self.insert_member_references(referencers, object_type_reference.deref())?;
        }
        self.introspection_typename_field()
            .insert_references(referencers)
    }

    fn remove_references(&self, type_: &Node<UnionType>, referencers: &mut Referencers) {
        for directive_reference in type_.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
        for object_type_reference in type_.members.iter() {
            self.remove_member_references(referencers, object_type_reference.deref());
        }
        self.introspection_typename_field()
            .remove_references(referencers)
    }

    fn insert_directive_name_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let directive_referencers = referencers.directives.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Union type \"{self}\"'s directive application \"@{name}\" does not refer to an existing directive.",
                ),
            }
        })?;
        directive_referencers.union_types.insert(self.clone());
        Ok(())
    }

    fn remove_directive_name_references(&self, referencers: &mut Referencers, name: &str) {
        let Some(directive_referencers) = referencers.directives.get_mut(name) else {
            return;
        };
        directive_referencers.union_types.shift_remove(self);
    }

    fn insert_member_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let object_type_referencers = referencers.object_types.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Union type \"{self}\"'s member \"{name}\" does not refer to an existing object.",
                ),
            }
        })?;
        object_type_referencers.union_types.insert(self.clone());
        Ok(())
    }

    fn remove_member_references(&self, referencers: &mut Referencers, name: &str) {
        let Some(object_type_referencers) = referencers.object_types.get_mut(name) else {
            return;
        };
        object_type_referencers.union_types.shift_remove(self);
    }

    fn rename(&self, schema: &mut FederationSchema, new_name: Name) -> Result<(), FederationError> {
        self.make_mut(&mut schema.schema)?.make_mut().name = new_name.clone();

        if let Some(union_type_referencers) =
            schema.referencers.union_types.swap_remove(&self.type_name)
        {
            schema
                .referencers
                .union_types
                .insert(new_name, union_type_referencers);
        }

        Ok(())
    }

    fn rename_member(
        &self,
        schema: &mut FederationSchema,
        old_name: &Name,
        new_name: Name,
    ) -> Result<(), FederationError> {
        let type_ = self.make_mut(&mut schema.schema)?.make_mut();
        type_.members.swap_remove(old_name);
        type_.members.insert(new_name.into());

        Ok(())
    }

    fn remove_extensions(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let type_ = self.make_mut(&mut schema.schema)?.make_mut();
        for directive in type_.directives.iter_mut() {
            directive.origin = ComponentOrigin::Definition;
        }
        type_.members = type_
            .members
            .iter()
            .map(|m| {
                let mut m = m.clone();
                m.origin = ComponentOrigin::Definition;
                m
            })
            .collect();
        Ok(())
    }

    pub(crate) fn has_applied_directive(
        &self,
        schema: &FederationSchema,
        directive_name: &Name,
    ) -> bool {
        if let Some(type_) = self.try_get(schema.schema()) {
            return type_
                .directives
                .iter()
                .any(|directive| &directive.name == directive_name);
        }
        false
    }

    pub(crate) fn get_all_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<impl Iterator<Item = &'schema Component<Directive>>, FederationError> {
        Ok(self.get(&schema.schema)?.directives.iter())
    }

    pub(crate) fn get_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
        directive_name: &Name,
    ) -> Vec<&'schema Component<Directive>> {
        if let Some(field) = self.try_get(&schema.schema) {
            field
                .directives
                .iter()
                .filter(|directive| &directive.name == directive_name)
                .collect()
        } else {
            Vec::new()
        }
    }

    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Component<Directive>,
    ) {
        let Some(obj) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        if !obj.directives.iter().any(|other_directive| {
            (other_directive.name == directive.name) && !other_directive.ptr_eq(directive)
        }) {
            self.remove_directive_name_references(&mut schema.referencers, &directive.name);
        }
        obj.make_mut()
            .directives
            .retain(|other_directive| !other_directive.ptr_eq(directive));
    }
}

impl Display for UnionTypeDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.type_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub(crate) struct UnionTypenameFieldDefinitionPosition {
    pub(crate) type_name: Name,
}

impl UnionTypenameFieldDefinitionPosition {
    pub(crate) fn field_name(&self) -> &Name {
        &INTROSPECTION_TYPENAME_FIELD_NAME
    }

    pub(crate) fn parent(&self) -> UnionTypeDefinitionPosition {
        UnionTypeDefinitionPosition {
            type_name: self.type_name.clone(),
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Component<FieldDefinition>, PositionLookupError> {
        let parent = self.parent();
        parent.get(schema)?;

        schema
            .type_field(&self.type_name, self.field_name())
            .map_err(|_| {
                PositionLookupError::MissingField(
                    "Union",
                    self.type_name.clone(),
                    name!("__typename"),
                )
            })
    }

    fn insert_references(&self, referencers: &mut Referencers) -> Result<(), FederationError> {
        self.insert_type_references(referencers)?;
        Ok(())
    }

    fn remove_references(&self, referencers: &mut Referencers) {
        self.remove_type_references(referencers);
    }

    fn insert_type_references(&self, referencers: &mut Referencers) -> Result<(), FederationError> {
        let output_type_reference = "String";
        if let Some(scalar_type_referencers) =
            referencers.scalar_types.get_mut(output_type_reference)
        {
            scalar_type_referencers.union_fields.insert(self.clone());
        } else {
            return Err(FederationError::internal(format!(
                "Schema missing referencers for type \"{output_type_reference}\""
            )));
        }
        Ok(())
    }

    fn remove_type_references(&self, referencers: &mut Referencers) {
        let output_type_reference = "String";
        if let Some(scalar_type_referencers) =
            referencers.scalar_types.get_mut(output_type_reference)
        {
            scalar_type_referencers.union_fields.shift_remove(self);
        }
    }
}

impl Display for UnionTypenameFieldDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.type_name, self.field_name())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct EnumTypeDefinitionPosition {
    pub(crate) type_name: Name,
}

impl EnumTypeDefinitionPosition {
    const EXPECTED: &'static str = "an enum type";

    pub(crate) fn value(&self, value_name: Name) -> EnumValueDefinitionPosition {
        EnumValueDefinitionPosition {
            type_name: self.type_name.clone(),
            value_name,
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<EnumType>, PositionLookupError> {
        schema
            .types
            .get(&self.type_name)
            .ok_or_else(|| PositionLookupError::TypeMissing(self.type_name.clone()))
            .and_then(|type_| {
                if let ExtendedType::Enum(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(PositionLookupError::TypeWrongKind(
                        self.type_name.clone(),
                        Self::EXPECTED,
                    ))
                }
            })
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema Node<EnumType>> {
        self.get(schema).ok()
    }

    fn make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Result<&'schema mut Node<EnumType>, PositionLookupError> {
        schema
            .types
            .get_mut(&self.type_name)
            .ok_or_else(|| PositionLookupError::TypeMissing(self.type_name.clone()))
            .and_then(|type_| {
                if let ExtendedType::Enum(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(PositionLookupError::TypeWrongKind(
                        self.type_name.clone(),
                        Self::EXPECTED,
                    ))
                }
            })
    }

    fn try_make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Option<&'schema mut Node<EnumType>> {
        if self.try_get(schema).is_some() {
            self.make_mut(schema).ok()
        } else {
            None
        }
    }

    pub(crate) fn pre_insert(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        if schema.referencers.contains_type_name(&self.type_name) {
            bail!(r#"Type "{self}" has already been pre-inserted"#);
        }
        schema
            .referencers
            .enum_types
            .insert(self.type_name.clone(), Default::default());
        Ok(())
    }

    pub(crate) fn insert(
        &self,
        schema: &mut FederationSchema,
        type_: Node<EnumType>,
    ) -> Result<(), FederationError> {
        if self.type_name != type_.name {
            return Err(SingleFederationError::Internal {
                message: format!("Enum type \"{}\" given type named \"{}\"", self, type_.name,),
            }
            .into());
        }
        if !schema.referencers.enum_types.contains_key(&self.type_name) {
            bail!(r#"Type "{self}" has not been pre-inserted"#);
        }
        if schema.schema.types.contains_key(&self.type_name) {
            bail!(r#"Type "{self}" already exists in schema"#);
        }
        schema
            .schema
            .types
            .insert(self.type_name.clone(), ExtendedType::Enum(type_));
        self.insert_references(self.get(&schema.schema)?, &mut schema.referencers)
    }

    /// Remove this enum from the schema, and remove its direct references.
    ///
    /// This can make the schema invalid if a field referencing the enum is the last of field in
    /// its type. That would cause the type to become empty.
    pub(crate) fn remove(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<Option<EnumTypeReferencers>, FederationError> {
        let Some(referencers) = self.remove_internal(schema)? else {
            return Ok(None);
        };
        for field in &referencers.object_fields {
            field.remove(schema)?;
        }
        for argument in &referencers.object_field_arguments {
            argument.remove(schema)?;
        }
        for field in &referencers.interface_fields {
            field.remove(schema)?;
        }
        for argument in &referencers.interface_field_arguments {
            argument.remove(schema)?;
        }
        for field in &referencers.input_object_fields {
            field.remove(schema)?;
        }
        for argument in &referencers.directive_arguments {
            argument.remove(schema)?;
        }
        Ok(Some(referencers))
    }

    fn remove_internal(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<Option<EnumTypeReferencers>, FederationError> {
        let Some(type_) = self.try_get(&schema.schema) else {
            return Ok(None);
        };
        self.remove_references(type_, &mut schema.referencers)?;
        schema.schema.types.shift_remove(&self.type_name);
        Ok(Some(
            schema
                .referencers
                .enum_types
                .shift_remove(&self.type_name)
                .ok_or_else(|| SingleFederationError::Internal {
                    message: format!("Schema missing referencers for type \"{self}\""),
                })?,
        ))
    }

    pub(crate) fn insert_directive(
        &self,
        schema: &mut FederationSchema,
        directive: Component<Directive>,
    ) -> Result<(), FederationError> {
        let type_ = self.make_mut(&mut schema.schema)?;
        if type_
            .directives
            .iter()
            .any(|other_directive| other_directive.ptr_eq(&directive))
        {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Directive application \"@{}\" already exists on enum type \"{}\"",
                    directive.name, self,
                ),
            }
            .into());
        }
        let name = directive.name.clone();
        type_.make_mut().directives.push(directive);
        self.insert_directive_name_references(&mut schema.referencers, &name)
    }

    /// Remove a directive application from this position by name.
    pub(crate) fn remove_directive_name(&self, schema: &mut FederationSchema, name: &str) {
        let Some(type_) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        self.remove_directive_name_references(&mut schema.referencers, name);
        type_
            .make_mut()
            .directives
            .retain(|other_directive| other_directive.name != name);
    }

    fn insert_references(
        &self,
        type_: &Node<EnumType>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        validate_component_directives(type_.directives.deref())?;
        for directive_reference in type_.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        for (value_name, value) in type_.values.iter() {
            self.value(value_name.clone())
                .insert_references(value, referencers)?;
        }
        Ok(())
    }

    fn remove_references(
        &self,
        type_: &Node<EnumType>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        for directive_reference in type_.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
        for (value_name, value) in type_.values.iter() {
            self.value(value_name.clone())
                .remove_references(value, referencers)?;
        }
        Ok(())
    }

    fn insert_directive_name_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let directive_referencers = referencers.directives.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Enum type \"{self}\"'s directive application \"@{name}\" does not refer to an existing directive.",
                ),
            }
        })?;
        directive_referencers.enum_types.insert(self.clone());
        Ok(())
    }

    fn remove_directive_name_references(&self, referencers: &mut Referencers, name: &str) {
        let Some(directive_referencers) = referencers.directives.get_mut(name) else {
            return;
        };
        directive_referencers.enum_types.shift_remove(self);
    }

    fn rename(&self, schema: &mut FederationSchema, new_name: Name) -> Result<(), FederationError> {
        self.make_mut(&mut schema.schema)?.make_mut().name = new_name.clone();

        if let Some(enum_type_referencers) =
            schema.referencers.enum_types.swap_remove(&self.type_name)
        {
            for pos in enum_type_referencers.object_fields.iter() {
                pos.rename_type(schema, new_name.clone())?;
            }
            for pos in enum_type_referencers.object_field_arguments.iter() {
                pos.rename_type(schema, new_name.clone())?;
            }
            for pos in enum_type_referencers.interface_fields.iter() {
                pos.rename_type(schema, new_name.clone())?;
            }
            for pos in enum_type_referencers.interface_field_arguments.iter() {
                pos.rename_type(schema, new_name.clone())?;
            }
            for pos in enum_type_referencers.input_object_fields.iter() {
                pos.rename_type(schema, new_name.clone())?;
            }
            schema
                .referencers
                .enum_types
                .insert(new_name, enum_type_referencers);
        }

        Ok(())
    }

    fn remove_extensions(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let type_ = self.make_mut(&mut schema.schema)?.make_mut();
        for directive in type_.directives.iter_mut() {
            directive.origin = ComponentOrigin::Definition;
        }
        for (_, v) in type_.values.iter_mut() {
            v.origin = ComponentOrigin::Definition;
        }
        Ok(())
    }

    pub(crate) fn has_applied_directive(
        &self,
        schema: &FederationSchema,
        directive_name: &Name,
    ) -> bool {
        if let Some(type_) = self.try_get(schema.schema()) {
            return type_
                .directives
                .iter()
                .any(|directive| &directive.name == directive_name);
        }
        false
    }

    pub(crate) fn get_all_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<impl Iterator<Item = &'schema Component<Directive>>, FederationError> {
        Ok(self.get(&schema.schema)?.directives.iter())
    }

    pub(crate) fn get_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
        directive_name: &Name,
    ) -> Vec<&'schema Component<Directive>> {
        if let Some(field) = self.try_get(&schema.schema) {
            field
                .directives
                .iter()
                .filter(|directive| &directive.name == directive_name)
                .collect()
        } else {
            Vec::new()
        }
    }

    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Component<Directive>,
    ) {
        let Some(obj) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        if !obj.directives.iter().any(|other_directive| {
            (other_directive.name == directive.name) && !other_directive.ptr_eq(directive)
        }) {
            self.remove_directive_name_references(&mut schema.referencers, &directive.name);
        }
        obj.make_mut()
            .directives
            .retain(|other_directive| !other_directive.ptr_eq(directive));
    }
}

impl Display for EnumTypeDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.type_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct EnumValueDefinitionPosition {
    pub(crate) type_name: Name,
    pub(crate) value_name: Name,
}

impl EnumValueDefinitionPosition {
    pub(crate) fn get_all_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<impl Iterator<Item = &'schema Node<Directive>>, FederationError> {
        Ok(self.get(&schema.schema)?.directives.iter())
    }

    pub(crate) fn get_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
        directive_name: &Name,
    ) -> Vec<&'schema Node<Directive>> {
        if let Some(val) = self.try_get(&schema.schema) {
            val.directives
                .iter()
                .filter(|d| &d.name == directive_name)
                .collect()
        } else {
            Vec::new()
        }
    }
    pub(crate) fn parent(&self) -> EnumTypeDefinitionPosition {
        EnumTypeDefinitionPosition {
            type_name: self.type_name.clone(),
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Component<EnumValueDefinition>, PositionLookupError> {
        let parent = self.parent();
        let type_ = parent.get(schema)?;

        type_
            .values
            .get(&self.value_name)
            .ok_or_else(|| PositionLookupError::MissingValue(self.clone()))
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema Component<EnumValueDefinition>> {
        self.get(schema).ok()
    }

    fn make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Result<&'schema mut Component<EnumValueDefinition>, PositionLookupError> {
        let parent = self.parent();
        let type_ = parent.make_mut(schema)?.make_mut();

        type_
            .values
            .get_mut(&self.value_name)
            .ok_or_else(|| PositionLookupError::MissingValue(self.clone()))
    }

    fn try_make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Option<&'schema mut Component<EnumValueDefinition>> {
        if self.try_get(schema).is_some() {
            self.make_mut(schema).ok()
        } else {
            None
        }
    }

    pub(crate) fn insert(
        &self,
        schema: &mut FederationSchema,
        value: Component<EnumValueDefinition>,
    ) -> Result<(), FederationError> {
        if self.value_name != value.value {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Enum value \"{}\" given argument named \"{}\"",
                    self, value.value,
                ),
            }
            .into());
        }
        if self.try_get(&schema.schema).is_some() {
            bail!(r#"Enum value "{self}" already exists in schema"#);
        }
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .values
            .insert(self.value_name.clone(), value);
        self.insert_references(self.get(&schema.schema)?, &mut schema.referencers)
    }

    /// Remove this value from the enum definition.
    ///
    /// This can make the schema invalid if the enum value is used in any directive applications,
    /// or if the value is the only value in its enum definition.
    pub(crate) fn remove(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let Some(value) = self.try_get(&schema.schema) else {
            return Ok(());
        };
        self.remove_references(value, &mut schema.referencers)?;
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .values
            .shift_remove(&self.value_name);
        Ok(())
    }

    pub(crate) fn insert_directive(
        &self,
        schema: &mut FederationSchema,
        directive: Node<Directive>,
    ) -> Result<(), FederationError> {
        let value = self.make_mut(&mut schema.schema)?;
        if value
            .directives
            .iter()
            .any(|other_directive| other_directive.ptr_eq(&directive))
        {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Directive application \"@{}\" already exists on enum value \"{}\"",
                    directive.name, self,
                ),
            }
            .into());
        }
        let name = directive.name.clone();
        value.make_mut().directives.push(directive);
        self.insert_directive_name_references(&mut schema.referencers, &name)
    }

    /// Remove a directive application from this position by name.
    pub(crate) fn remove_directive_name(&self, schema: &mut FederationSchema, name: &str) {
        let Some(value) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        self.remove_directive_name_references(&mut schema.referencers, name);
        value
            .make_mut()
            .directives
            .retain(|other_directive| other_directive.name != name);
    }

    fn insert_references(
        &self,
        value: &Component<EnumValueDefinition>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if is_graphql_reserved_name(&self.value_name) {
            bail!(r#"Cannot insert reserved enum value "{self}""#);
        }
        validate_node_directives(value.directives.deref())?;
        for directive_reference in value.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        Ok(())
    }

    fn remove_references(
        &self,
        value: &Component<EnumValueDefinition>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if is_graphql_reserved_name(&self.value_name) {
            bail!(r#"Cannot remove reserved enum value "{self}""#);
        }
        for directive_reference in value.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
        Ok(())
    }

    fn insert_directive_name_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let directive_referencers = referencers.directives.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Enum value \"{self}\"'s directive application \"@{name}\" does not refer to an existing directive.",
                ),
            }
        })?;
        directive_referencers.enum_values.insert(self.clone());
        Ok(())
    }

    fn remove_directive_name_references(&self, referencers: &mut Referencers, name: &str) {
        let Some(directive_referencers) = referencers.directives.get_mut(name) else {
            return;
        };
        directive_referencers.enum_values.shift_remove(self);
    }
}

impl Display for EnumValueDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.type_name, self.value_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct InputObjectTypeDefinitionPosition {
    pub(crate) type_name: Name,
}

impl InputObjectTypeDefinitionPosition {
    const EXPECTED: &'static str = "an input object type";

    pub(crate) fn field(&self, field_name: Name) -> InputObjectFieldDefinitionPosition {
        InputObjectFieldDefinitionPosition {
            type_name: self.type_name.clone(),
            field_name,
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<InputObjectType>, PositionLookupError> {
        schema
            .types
            .get(&self.type_name)
            .ok_or_else(|| PositionLookupError::TypeMissing(self.type_name.clone()))
            .and_then(|type_| {
                if let ExtendedType::InputObject(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(PositionLookupError::TypeWrongKind(
                        self.type_name.clone(),
                        Self::EXPECTED,
                    ))
                }
            })
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema Node<InputObjectType>> {
        self.get(schema).ok()
    }

    fn make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Result<&'schema mut Node<InputObjectType>, PositionLookupError> {
        schema
            .types
            .get_mut(&self.type_name)
            .ok_or_else(|| PositionLookupError::TypeMissing(self.type_name.clone()))
            .and_then(|type_| {
                if let ExtendedType::InputObject(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(PositionLookupError::TypeWrongKind(
                        self.type_name.clone(),
                        Self::EXPECTED,
                    ))
                }
            })
    }

    fn try_make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Option<&'schema mut Node<InputObjectType>> {
        if self.try_get(schema).is_some() {
            self.make_mut(schema).ok()
        } else {
            None
        }
    }

    pub(crate) fn pre_insert(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        if schema.referencers.contains_type_name(&self.type_name) {
            bail!(r#"Type "{self}" has already been pre-inserted"#);
        }
        schema
            .referencers
            .input_object_types
            .insert(self.type_name.clone(), Default::default());
        Ok(())
    }

    pub(crate) fn insert(
        &self,
        schema: &mut FederationSchema,
        type_: Node<InputObjectType>,
    ) -> Result<(), FederationError> {
        if self.type_name != type_.name {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Input object type \"{}\" given type named \"{}\"",
                    self, type_.name,
                ),
            }
            .into());
        }
        if !schema
            .referencers
            .input_object_types
            .contains_key(&self.type_name)
        {
            bail!(r#"Type "{self}" has not been pre-inserted"#);
        }
        if schema.schema.types.contains_key(&self.type_name) {
            bail!(r#"Type "{self}" already exists in schema"#);
        }
        schema
            .schema
            .types
            .insert(self.type_name.clone(), ExtendedType::InputObject(type_));
        self.insert_references(self.get(&schema.schema)?, &mut schema.referencers)
    }

    /// Remove this input type from the schema.
    ///
    /// TODO document validity
    pub(crate) fn remove(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<Option<InputObjectTypeReferencers>, FederationError> {
        let Some(referencers) = self.remove_internal(schema)? else {
            return Ok(None);
        };
        for argument in &referencers.object_field_arguments {
            argument.remove(schema)?;
        }
        for argument in &referencers.interface_field_arguments {
            argument.remove(schema)?;
        }
        for field in &referencers.input_object_fields {
            field.remove(schema)?;
        }
        for argument in &referencers.directive_arguments {
            argument.remove(schema)?;
        }
        Ok(Some(referencers))
    }

    /// Remove this input type from the schema.
    ///
    /// TODO document validity
    pub(crate) fn remove_recursive(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        let Some(referencers) = self.remove_internal(schema)? else {
            return Ok(());
        };
        for argument in referencers.object_field_arguments {
            argument.remove(schema)?;
        }
        for argument in referencers.interface_field_arguments {
            argument.remove(schema)?;
        }
        for field in referencers.input_object_fields {
            field.remove_recursive(schema)?;
        }
        for argument in referencers.directive_arguments {
            argument.remove(schema)?;
        }
        Ok(())
    }

    fn remove_internal(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<Option<InputObjectTypeReferencers>, FederationError> {
        let Some(type_) = self.try_get(&schema.schema) else {
            return Ok(None);
        };
        self.remove_references(type_, &mut schema.referencers)?;
        schema.schema.types.shift_remove(&self.type_name);
        Ok(Some(
            schema
                .referencers
                .input_object_types
                .shift_remove(&self.type_name)
                .ok_or_else(|| SingleFederationError::Internal {
                    message: format!("Schema missing referencers for type \"{self}\""),
                })?,
        ))
    }

    pub(crate) fn insert_directive(
        &self,
        schema: &mut FederationSchema,
        directive: Component<Directive>,
    ) -> Result<(), FederationError> {
        let type_ = self.make_mut(&mut schema.schema)?;
        if type_
            .directives
            .iter()
            .any(|other_directive| other_directive.ptr_eq(&directive))
        {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Directive application \"@{}\" already exists on input object type \"{}\"",
                    directive.name, self,
                ),
            }
            .into());
        }
        let name = directive.name.clone();
        type_.make_mut().directives.push(directive);
        self.insert_directive_name_references(&mut schema.referencers, &name)
    }

    /// Remove a directive application from this position by name.
    pub(crate) fn remove_directive_name(&self, schema: &mut FederationSchema, name: &str) {
        let Some(type_) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        self.remove_directive_name_references(&mut schema.referencers, name);
        type_
            .make_mut()
            .directives
            .retain(|other_directive| other_directive.name != name);
    }

    fn insert_references(
        &self,
        type_: &Node<InputObjectType>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        validate_component_directives(type_.directives.deref())?;
        for directive_reference in type_.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        for (field_name, field) in type_.fields.iter() {
            self.field(field_name.clone())
                .insert_references(field, referencers)?;
        }
        Ok(())
    }

    fn remove_references(
        &self,
        type_: &Node<InputObjectType>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        for directive_reference in type_.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
        for (field_name, field) in type_.fields.iter() {
            self.field(field_name.clone())
                .remove_references(field, referencers)?;
        }
        Ok(())
    }

    fn insert_directive_name_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let directive_referencers = referencers.directives.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Input object type \"{self}\"'s directive application \"@{name}\" does not refer to an existing directive.",
                ),
            }
        })?;
        directive_referencers
            .input_object_types
            .insert(self.clone());
        Ok(())
    }

    fn remove_directive_name_references(&self, referencers: &mut Referencers, name: &str) {
        let Some(directive_referencers) = referencers.directives.get_mut(name) else {
            return;
        };
        directive_referencers.input_object_types.shift_remove(self);
    }

    fn rename(&self, schema: &mut FederationSchema, new_name: Name) -> Result<(), FederationError> {
        self.make_mut(&mut schema.schema)?.make_mut().name = new_name.clone();

        if let Some(input_object_type_referencers) = schema
            .referencers
            .input_object_types
            .swap_remove(&self.type_name)
        {
            schema
                .referencers
                .input_object_types
                .insert(new_name, input_object_type_referencers);
        }

        Ok(())
    }

    fn remove_extensions(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let type_ = self.make_mut(&mut schema.schema)?.make_mut();
        for directive in type_.directives.iter_mut() {
            directive.origin = ComponentOrigin::Definition;
        }
        for (_, field) in type_.fields.iter_mut() {
            field.origin = ComponentOrigin::Definition;
        }
        Ok(())
    }

    pub(crate) fn has_applied_directive(
        &self,
        schema: &FederationSchema,
        directive_name: &Name,
    ) -> bool {
        if let Some(type_) = self.try_get(schema.schema()) {
            return type_
                .directives
                .iter()
                .any(|directive| &directive.name == directive_name);
        }
        false
    }

    pub(crate) fn get_all_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<impl Iterator<Item = &'schema Component<Directive>>, FederationError> {
        Ok(self.get(&schema.schema)?.directives.iter())
    }

    pub(crate) fn get_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
        directive_name: &Name,
    ) -> Vec<&'schema Component<Directive>> {
        if let Some(field) = self.try_get(&schema.schema) {
            field
                .directives
                .iter()
                .filter(|directive| &directive.name == directive_name)
                .collect()
        } else {
            Vec::new()
        }
    }

    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Component<Directive>,
    ) {
        let Some(obj) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        if !obj.directives.iter().any(|other_directive| {
            (other_directive.name == directive.name) && !other_directive.ptr_eq(directive)
        }) {
            self.remove_directive_name_references(&mut schema.referencers, &directive.name);
        }
        obj.make_mut()
            .directives
            .retain(|other_directive| !other_directive.ptr_eq(directive));
    }
}

impl Display for InputObjectTypeDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.type_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct InputObjectFieldDefinitionPosition {
    pub(crate) type_name: Name,
    pub(crate) field_name: Name,
}

impl InputObjectFieldDefinitionPosition {
    pub(crate) fn get_all_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<impl Iterator<Item = &'schema Node<Directive>>, FederationError> {
        Ok(self.get(&schema.schema)?.directives.iter())
    }

    pub(crate) fn get_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
        directive_name: &Name,
    ) -> Vec<&'schema Node<Directive>> {
        if let Some(field) = self.try_get(&schema.schema) {
            field
                .directives
                .iter()
                .filter(|d| &d.name == directive_name)
                .collect()
        } else {
            Vec::new()
        }
    }

    pub(crate) fn has_applied_directive(
        &self,
        schema: &FederationSchema,
        directive_name: &Name,
    ) -> bool {
        !self
            .get_applied_directives(schema, directive_name)
            .is_empty()
    }

    pub(crate) fn parent(&self) -> InputObjectTypeDefinitionPosition {
        InputObjectTypeDefinitionPosition {
            type_name: self.type_name.clone(),
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Component<InputValueDefinition>, PositionLookupError> {
        let parent = self.parent();
        let type_ = parent.get(schema)?;

        type_.fields.get(&self.field_name).ok_or_else(|| {
            PositionLookupError::MissingField(
                "Input object",
                self.type_name.clone(),
                self.field_name.clone(),
            )
        })
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema Component<InputValueDefinition>> {
        self.get(schema).ok()
    }

    fn make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Result<&'schema mut Component<InputValueDefinition>, PositionLookupError> {
        let parent = self.parent();
        let type_ = parent.make_mut(schema)?.make_mut();

        type_.fields.get_mut(&self.field_name).ok_or_else(|| {
            PositionLookupError::MissingField(
                "Input object",
                self.type_name.clone(),
                self.field_name.clone(),
            )
        })
    }

    fn try_make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Option<&'schema mut Component<InputValueDefinition>> {
        if self.try_get(schema).is_some() {
            self.make_mut(schema).ok()
        } else {
            None
        }
    }

    pub(crate) fn insert(
        &self,
        schema: &mut FederationSchema,
        field: Component<InputValueDefinition>,
    ) -> Result<(), FederationError> {
        if self.field_name != field.name {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Input object field \"{}\" given field named \"{}\"",
                    self, field.name,
                ),
            }
            .into());
        }
        if self.try_get(&schema.schema).is_some() {
            bail!(r#"Input object field "{self}" already exists in schema"#);
        }
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .fields
            .insert(self.field_name.clone(), field);
        self.insert_references(self.get(&schema.schema)?, &mut schema.referencers)
    }

    /// Remove this field from its input object type.
    ///
    /// TODO document validity
    pub(crate) fn remove(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let Some(field) = self.try_get(&schema.schema) else {
            return Ok(());
        };
        self.remove_references(field, &mut schema.referencers)?;
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .fields
            .shift_remove(&self.field_name);
        Ok(())
    }

    /// Remove this field from its input object type.
    ///
    /// TODO document validity
    pub(crate) fn remove_recursive(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        self.remove(schema)?;
        let parent = self.parent();
        let Some(type_) = parent.try_get(&schema.schema) else {
            return Ok(());
        };
        if type_.fields.is_empty() {
            parent.remove_recursive(schema)?;
        }
        Ok(())
    }

    pub(crate) fn insert_directive(
        &self,
        schema: &mut FederationSchema,
        directive: Node<Directive>,
    ) -> Result<(), FederationError> {
        let field = self.make_mut(&mut schema.schema)?;
        if field
            .directives
            .iter()
            .any(|other_directive| other_directive.ptr_eq(&directive))
        {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Directive application \"@{}\" already exists on input object field \"{}\"",
                    directive.name, self,
                ),
            }
            .into());
        }
        let name = directive.name.clone();
        field.make_mut().directives.push(directive);
        self.insert_directive_name_references(&mut schema.referencers, &name)
    }

    /// Remove a directive application from this position by name.
    pub(crate) fn remove_directive_name(&self, schema: &mut FederationSchema, name: &str) {
        let Some(field) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        self.remove_directive_name_references(&mut schema.referencers, name);
        field
            .make_mut()
            .directives
            .retain(|other_directive| other_directive.name != name);
    }

    fn insert_references(
        &self,
        field: &Component<InputValueDefinition>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if is_graphql_reserved_name(&self.field_name) {
            bail!(r#"Cannot insert reserved input object field "{self}""#);
        }
        validate_node_directives(field.directives.deref())?;
        for directive_reference in field.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        self.insert_type_references(field, referencers)
    }

    fn remove_references(
        &self,
        field: &Component<InputValueDefinition>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if is_graphql_reserved_name(&self.field_name) {
            bail!(r#"Cannot remove reserved input object field "{self}""#);
        }
        for directive_reference in field.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
        self.remove_type_references(field, referencers);
        Ok(())
    }

    fn insert_directive_name_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let directive_referencers = referencers.directives.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Input object field \"{self}\"'s directive application \"@{name}\" does not refer to an existing directive.",
                ),
            }
        })?;
        directive_referencers
            .input_object_fields
            .insert(self.clone());
        Ok(())
    }

    fn remove_directive_name_references(&self, referencers: &mut Referencers, name: &str) {
        let Some(directive_referencers) = referencers.directives.get_mut(name) else {
            return;
        };
        directive_referencers.input_object_fields.shift_remove(self);
    }

    fn insert_type_references(
        &self,
        argument: &Node<InputValueDefinition>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let input_type_reference = argument.ty.inner_named_type();
        if let Some(scalar_type_referencers) =
            referencers.scalar_types.get_mut(input_type_reference)
        {
            scalar_type_referencers
                .input_object_fields
                .insert(self.clone());
        } else if let Some(enum_type_referencers) =
            referencers.enum_types.get_mut(input_type_reference)
        {
            enum_type_referencers
                .input_object_fields
                .insert(self.clone());
        } else if let Some(input_object_type_referencers) =
            referencers.input_object_types.get_mut(input_type_reference)
        {
            input_object_type_referencers
                .input_object_fields
                .insert(self.clone());
        } else {
            return Err(FederationError::internal(format!(
                "Input object field \"{}\"'s inner type \"{}\" does not refer to an existing input type.",
                self,
                input_type_reference.deref(),
            )));
        }
        Ok(())
    }

    fn remove_type_references(
        &self,
        field: &Component<InputValueDefinition>,
        referencers: &mut Referencers,
    ) {
        let input_type_reference = field.ty.inner_named_type();
        if let Some(scalar_type_referencers) =
            referencers.scalar_types.get_mut(input_type_reference)
        {
            scalar_type_referencers
                .input_object_fields
                .shift_remove(self);
        } else if let Some(enum_type_referencers) =
            referencers.enum_types.get_mut(input_type_reference)
        {
            enum_type_referencers.input_object_fields.shift_remove(self);
        } else if let Some(input_object_type_referencers) =
            referencers.input_object_types.get_mut(input_type_reference)
        {
            input_object_type_referencers
                .input_object_fields
                .shift_remove(self);
        }
    }

    fn rename_type(
        &self,
        schema: &mut FederationSchema,
        new_name: Name,
    ) -> Result<(), FederationError> {
        self.make_mut(&mut schema.schema)?.make_mut().name = new_name;
        Ok(())
    }
}

impl Display for InputObjectFieldDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.type_name, self.field_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DirectiveDefinitionPosition {
    pub(crate) directive_name: Name,
}

impl DirectiveDefinitionPosition {
    pub(crate) fn argument(&self, argument_name: Name) -> DirectiveArgumentDefinitionPosition {
        DirectiveArgumentDefinitionPosition {
            directive_name: self.directive_name.clone(),
            argument_name,
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<DirectiveDefinition>, PositionLookupError> {
        schema
            .directive_definitions
            .get(&self.directive_name)
            .ok_or_else(|| PositionLookupError::DirectiveMissing(self.clone()))
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema Node<DirectiveDefinition>> {
        self.get(schema).ok()
    }

    fn make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Result<&'schema mut Node<DirectiveDefinition>, PositionLookupError> {
        schema
            .directive_definitions
            .get_mut(&self.directive_name)
            .ok_or_else(|| PositionLookupError::DirectiveMissing(self.clone()))
    }

    pub(crate) fn pre_insert(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        if schema
            .referencers
            .directives
            .contains_key(&self.directive_name)
        {
            bail!(r#"Directive "{self}" has already been pre-inserted"#);
        }
        schema
            .referencers
            .directives
            .insert(self.directive_name.clone(), Default::default());
        Ok(())
    }

    pub(crate) fn insert(
        &self,
        schema: &mut FederationSchema,
        directive: Node<DirectiveDefinition>,
    ) -> Result<(), FederationError> {
        if !schema
            .referencers
            .directives
            .contains_key(&self.directive_name)
        {
            bail!(r#"Directive "{self}" has not been pre-inserted"#);
        }
        if schema
            .schema
            .directive_definitions
            .contains_key(&self.directive_name)
        {
            bail!(r#"Directive "{self}" already exists in schema"#);
        }
        schema
            .schema
            .directive_definitions
            .insert(self.directive_name.clone(), directive);
        self.insert_references(self.get(&schema.schema)?, &mut schema.referencers)
    }

    /// Remove the directive definition and any applications.
    pub(crate) fn remove(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<Option<DirectiveReferencers>, FederationError> {
        let Some(referencers) = self.remove_internal(schema)? else {
            return Ok(None);
        };
        if let Some(schema_definition) = &referencers.schema {
            schema_definition.remove_directive_name(schema, &self.directive_name)?;
        }
        for type_ in &referencers.scalar_types {
            type_.remove_directive_name(schema, &self.directive_name);
        }
        for type_ in &referencers.object_types {
            type_.remove_directive_name(schema, &self.directive_name);
        }
        for field in &referencers.object_fields {
            field.remove_directive_name(schema, &self.directive_name);
        }
        for argument in &referencers.object_field_arguments {
            argument.remove_directive_name(schema, &self.directive_name);
        }
        for type_ in &referencers.interface_types {
            type_.remove_directive_name(schema, &self.directive_name);
        }
        for field in &referencers.interface_fields {
            field.remove_directive_name(schema, &self.directive_name);
        }
        for argument in &referencers.interface_field_arguments {
            argument.remove_directive_name(schema, &self.directive_name);
        }
        for type_ in &referencers.union_types {
            type_.remove_directive_name(schema, &self.directive_name);
        }
        for type_ in &referencers.enum_types {
            type_.remove_directive_name(schema, &self.directive_name);
        }
        for value in &referencers.enum_values {
            value.remove_directive_name(schema, &self.directive_name);
        }
        for type_ in &referencers.input_object_types {
            type_.remove_directive_name(schema, &self.directive_name);
        }
        for field in &referencers.input_object_fields {
            field.remove_directive_name(schema, &self.directive_name);
        }
        for argument in &referencers.directive_arguments {
            argument.remove_directive_name(schema, &self.directive_name);
        }
        Ok(Some(referencers))
    }

    fn remove_internal(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<Option<DirectiveReferencers>, FederationError> {
        let Some(directive) = self.try_get(&schema.schema) else {
            return Ok(None);
        };
        self.remove_references(directive, &mut schema.referencers)?;
        schema
            .schema
            .directive_definitions
            .shift_remove(&self.directive_name);
        Ok(Some(
            schema
                .referencers
                .directives
                .shift_remove(&self.directive_name)
                .ok_or_else(|| SingleFederationError::Internal {
                    message: format!("Schema missing referencers for directive \"{self}\""),
                })?,
        ))
    }

    fn insert_references(
        &self,
        directive: &Node<DirectiveDefinition>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        for argument in directive.arguments.iter() {
            self.argument(argument.name.clone())
                .insert_references(argument, referencers)?;
        }
        Ok(())
    }

    fn remove_references(
        &self,
        directive: &Node<DirectiveDefinition>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        for argument in directive.arguments.iter() {
            self.argument(argument.name.clone())
                .remove_references(argument, referencers)?;
        }
        Ok(())
    }
}

impl Display for DirectiveDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "@{}", self.directive_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DirectiveArgumentDefinitionPosition {
    pub(crate) directive_name: Name,
    pub(crate) argument_name: Name,
}

impl DirectiveArgumentDefinitionPosition {
    pub(crate) fn get_all_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Result<impl Iterator<Item = &'schema Node<Directive>>, FederationError> {
        Ok(self.get(&schema.schema)?.directives.iter())
    }

    pub(crate) fn get_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
        directive_name: &Name,
    ) -> Vec<&'schema Node<Directive>> {
        if let Some(argument) = self.try_get(&schema.schema) {
            argument
                .directives
                .iter()
                .filter(|d| &d.name == directive_name)
                .collect()
        } else {
            Vec::new()
        }
    }

    pub(crate) fn parent(&self) -> DirectiveDefinitionPosition {
        DirectiveDefinitionPosition {
            directive_name: self.directive_name.clone(),
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<InputValueDefinition>, PositionLookupError> {
        let parent = self.parent();
        let type_ = parent.get(schema)?;

        type_
            .arguments
            .iter()
            .find(|a| a.name == self.argument_name)
            .ok_or_else(|| PositionLookupError::MissingDirectiveArgument(self.clone()))
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema Node<InputValueDefinition>> {
        self.get(schema).ok()
    }

    fn make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Result<&'schema mut Node<InputValueDefinition>, PositionLookupError> {
        let parent = self.parent();
        let type_ = parent.make_mut(schema)?.make_mut();

        type_
            .arguments
            .iter_mut()
            .find(|a| a.name == self.argument_name)
            .ok_or_else(|| PositionLookupError::MissingDirectiveArgument(self.clone()))
    }

    fn try_make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Option<&'schema mut Node<InputValueDefinition>> {
        if self.try_get(schema).is_some() {
            self.make_mut(schema).ok()
        } else {
            None
        }
    }

    /// Remove this argument definition from its directive. Any applications of the directive that
    /// use this argument will become invalid.
    pub(crate) fn remove(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let Some(argument) = self.try_get(&schema.schema) else {
            return Ok(());
        };
        self.remove_references(argument, &mut schema.referencers)?;
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .arguments
            .retain(|other_argument| other_argument.name != self.argument_name);
        Ok(())
    }

    pub(crate) fn insert_directive(
        &self,
        schema: &mut FederationSchema,
        directive: Node<Directive>,
    ) -> Result<(), FederationError> {
        let argument = self.make_mut(&mut schema.schema)?;
        if argument
            .directives
            .iter()
            .any(|other_directive| other_directive.ptr_eq(&directive))
        {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Directive application \"@{}\" already exists on directive argument \"{}\"",
                    directive.name, self,
                ),
            }
            .into());
        }
        let name = directive.name.clone();
        argument.make_mut().directives.push(directive);
        self.insert_directive_name_references(&mut schema.referencers, &name)
    }

    /// Remove a directive application from this position by name.
    pub(crate) fn remove_directive_name(&self, schema: &mut FederationSchema, name: &str) {
        let Some(argument) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        self.remove_directive_name_references(&mut schema.referencers, name);
        argument
            .make_mut()
            .directives
            .retain(|other_directive| other_directive.name != name);
    }

    fn insert_references(
        &self,
        argument: &Node<InputValueDefinition>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if is_graphql_reserved_name(&self.argument_name) {
            bail!(r#"Cannot insert reserved directive argument "{self}""#);
        }
        validate_node_directives(argument.directives.deref())?;
        for directive_reference in argument.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        self.insert_type_references(argument, referencers)
    }

    fn remove_references(
        &self,
        argument: &Node<InputValueDefinition>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if is_graphql_reserved_name(&self.argument_name) {
            bail!(r#"Cannot remove reserved directive argument "{self}""#);
        }
        for directive_reference in argument.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
        self.remove_type_references(argument, referencers);
        Ok(())
    }

    fn insert_directive_name_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let directive_referencers = referencers.directives.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Directive argument \"{self}\"'s directive application \"@{name}\" does not refer to an existing directive.",
                ),
            }
        })?;
        directive_referencers
            .directive_arguments
            .insert(self.clone());
        Ok(())
    }

    fn remove_directive_name_references(&self, referencers: &mut Referencers, name: &str) {
        let Some(directive_referencers) = referencers.directives.get_mut(name) else {
            return;
        };
        directive_referencers.directive_arguments.shift_remove(self);
    }

    fn insert_type_references(
        &self,
        argument: &Node<InputValueDefinition>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let input_type_reference = argument.ty.inner_named_type();
        if let Some(scalar_type_referencers) =
            referencers.scalar_types.get_mut(input_type_reference)
        {
            scalar_type_referencers
                .directive_arguments
                .insert(self.clone());
        } else if let Some(enum_type_referencers) =
            referencers.enum_types.get_mut(input_type_reference)
        {
            enum_type_referencers
                .directive_arguments
                .insert(self.clone());
        } else if let Some(input_object_type_referencers) =
            referencers.input_object_types.get_mut(input_type_reference)
        {
            input_object_type_referencers
                .directive_arguments
                .insert(self.clone());
        } else {
            return Err(FederationError::internal(format!(
                "Directive argument \"{}\"'s inner type \"{}\" does not refer to an existing input type.",
                self,
                input_type_reference.deref(),
            )));
        }
        Ok(())
    }

    fn remove_type_references(
        &self,
        argument: &Node<InputValueDefinition>,
        referencers: &mut Referencers,
    ) {
        let input_type_reference = argument.ty.inner_named_type();
        if let Some(scalar_type_referencers) =
            referencers.scalar_types.get_mut(input_type_reference)
        {
            scalar_type_referencers
                .directive_arguments
                .shift_remove(self);
        } else if let Some(enum_type_referencers) =
            referencers.enum_types.get_mut(input_type_reference)
        {
            enum_type_referencers.directive_arguments.shift_remove(self);
        } else if let Some(input_object_type_referencers) =
            referencers.input_object_types.get_mut(input_type_reference)
        {
            input_object_type_referencers
                .directive_arguments
                .shift_remove(self);
        }
    }
}

impl Display for DirectiveArgumentDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "@{}({}:)", self.directive_name, self.argument_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, derive_more::Display)]
pub(crate) enum DirectiveTargetPosition {
    Schema(SchemaDefinitionPosition),
    ScalarType(ScalarTypeDefinitionPosition),
    ObjectType(ObjectTypeDefinitionPosition),
    ObjectField(ObjectFieldDefinitionPosition),
    ObjectFieldArgument(ObjectFieldArgumentDefinitionPosition),
    InterfaceType(InterfaceTypeDefinitionPosition),
    InterfaceField(InterfaceFieldDefinitionPosition),
    InterfaceFieldArgument(InterfaceFieldArgumentDefinitionPosition),
    UnionType(UnionTypeDefinitionPosition),
    EnumType(EnumTypeDefinitionPosition),
    EnumValue(EnumValueDefinitionPosition),
    InputObjectType(InputObjectTypeDefinitionPosition),
    InputObjectField(InputObjectFieldDefinitionPosition),
    DirectiveArgument(DirectiveArgumentDefinitionPosition),
}

impl DirectiveTargetPosition {
    pub(crate) fn get_all_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
    ) -> Vec<&'schema Node<Directive>> {
        match self {
            Self::Schema(pos) => pos
                .get_all_applied_directives(schema)
                .map(|component| &component.node)
                .collect(),
            Self::ScalarType(pos) => pos
                .get_all_applied_directives(schema)
                .map(|it| it.map(|component| &component.node).collect())
                .unwrap_or_default(),
            Self::ObjectType(pos) => pos
                .get_all_applied_directives(schema)
                .map(|it| it.map(|component| &component.node).collect())
                .unwrap_or_default(),
            Self::ObjectField(pos) => pos
                .get_all_applied_directives(schema)
                .map(|it| it.collect())
                .unwrap_or_default(),
            Self::ObjectFieldArgument(pos) => pos
                .get_all_applied_directives(schema)
                .map(|it| it.collect())
                .unwrap_or_default(),
            Self::InterfaceType(pos) => pos
                .get_all_applied_directives(schema)
                .map(|it| it.map(|component| &component.node).collect())
                .unwrap_or_default(),
            Self::InterfaceField(pos) => pos
                .get_all_applied_directives(schema)
                .map(|it| it.collect())
                .unwrap_or_default(),
            Self::InterfaceFieldArgument(pos) => pos
                .get_all_applied_directives(schema)
                .map(|it| it.collect())
                .unwrap_or_default(),
            Self::UnionType(pos) => pos
                .get_all_applied_directives(schema)
                .map(|it| it.map(|component| &component.node).collect())
                .unwrap_or_default(),
            Self::EnumType(pos) => pos
                .get_all_applied_directives(schema)
                .map(|it| it.map(|component| &component.node).collect())
                .unwrap_or_default(),
            Self::EnumValue(pos) => pos
                .get_all_applied_directives(schema)
                .map(|it| it.collect())
                .unwrap_or_default(),
            Self::InputObjectType(pos) => pos
                .get_all_applied_directives(schema)
                .map(|it| it.map(|component| &component.node).collect())
                .unwrap_or_default(),
            Self::InputObjectField(pos) => pos
                .get_all_applied_directives(schema)
                .map(|it| it.collect())
                .unwrap_or_default(),
            Self::DirectiveArgument(pos) => pos
                .get_all_applied_directives(schema)
                .map(|it| it.collect())
                .unwrap_or_default(),
        }
    }

    pub(crate) fn get_applied_directives<'schema>(
        &self,
        schema: &'schema FederationSchema,
        directive_name: &Name,
    ) -> Vec<&'schema Node<Directive>> {
        match self {
            Self::Schema(pos) => pos
                .get_applied_directives(schema, directive_name)
                .iter()
                .map(|d| &d.node)
                .collect(),
            Self::ScalarType(pos) => pos
                .get_applied_directives(schema, directive_name)
                .iter()
                .map(|d| &d.node)
                .collect(),
            Self::ObjectType(pos) => pos
                .get_applied_directives(schema, directive_name)
                .iter()
                .map(|d| &d.node)
                .collect(),
            Self::ObjectField(pos) => pos.get_applied_directives(schema, directive_name),
            Self::ObjectFieldArgument(pos) => pos.get_applied_directives(schema, directive_name),
            Self::InterfaceType(pos) => pos
                .get_applied_directives(schema, directive_name)
                .iter()
                .map(|d| &d.node)
                .collect(),
            Self::InterfaceField(pos) => pos.get_applied_directives(schema, directive_name),
            Self::InterfaceFieldArgument(pos) => pos.get_applied_directives(schema, directive_name),
            Self::UnionType(pos) => pos
                .get_applied_directives(schema, directive_name)
                .iter()
                .map(|d| &d.node)
                .collect(),
            Self::EnumType(pos) => pos
                .get_applied_directives(schema, directive_name)
                .iter()
                .map(|d| &d.node)
                .collect(),
            Self::EnumValue(pos) => pos.get_applied_directives(schema, directive_name),
            Self::InputObjectType(pos) => pos
                .get_applied_directives(schema, directive_name)
                .iter()
                .map(|d| &d.node)
                .collect(),
            Self::InputObjectField(pos) => pos.get_applied_directives(schema, directive_name),
            Self::DirectiveArgument(pos) => pos.get_applied_directives(schema, directive_name),
        }
    }

    pub(crate) fn insert_directive(
        &self,
        schema: &mut FederationSchema,
        directive: Directive,
    ) -> Result<(), FederationError> {
        match self {
            Self::Schema(pos) => pos.insert_directive(schema, Component::new(directive)),
            Self::ScalarType(pos) => pos.insert_directive(schema, Component::new(directive)),
            Self::ObjectType(pos) => pos.insert_directive(schema, Component::new(directive)),
            Self::ObjectField(pos) => pos.insert_directive(schema, Node::new(directive)),
            Self::ObjectFieldArgument(pos) => pos.insert_directive(schema, Node::new(directive)),
            Self::InterfaceType(pos) => pos.insert_directive(schema, Component::new(directive)),
            Self::InterfaceField(pos) => pos.insert_directive(schema, Node::new(directive)),
            Self::InterfaceFieldArgument(pos) => pos.insert_directive(schema, Node::new(directive)),
            Self::UnionType(pos) => pos.insert_directive(schema, Component::new(directive)),
            Self::EnumType(pos) => pos.insert_directive(schema, Component::new(directive)),
            Self::EnumValue(pos) => pos.insert_directive(schema, Node::new(directive)),
            Self::InputObjectType(pos) => pos.insert_directive(schema, Component::new(directive)),
            Self::InputObjectField(pos) => pos.insert_directive(schema, Node::new(directive)),
            Self::DirectiveArgument(pos) => pos.insert_directive(schema, Node::new(directive)),
        }
    }
}

impl From<ObjectOrInterfaceFieldDefinitionPosition> for DirectiveTargetPosition {
    fn from(pos: ObjectOrInterfaceFieldDefinitionPosition) -> Self {
        match pos {
            ObjectOrInterfaceFieldDefinitionPosition::Object(pos) => {
                DirectiveTargetPosition::ObjectField(pos)
            }
            ObjectOrInterfaceFieldDefinitionPosition::Interface(pos) => {
                DirectiveTargetPosition::InterfaceField(pos)
            }
        }
    }
}

impl From<ObjectTypeDefinitionPosition> for DirectiveTargetPosition {
    fn from(pos: ObjectTypeDefinitionPosition) -> Self {
        DirectiveTargetPosition::ObjectType(pos)
    }
}

impl From<SchemaDefinitionPosition> for DirectiveTargetPosition {
    fn from(pos: SchemaDefinitionPosition) -> Self {
        DirectiveTargetPosition::Schema(pos)
    }
}

impl From<TypeDefinitionPosition> for DirectiveTargetPosition {
    fn from(pos: TypeDefinitionPosition) -> Self {
        match pos {
            TypeDefinitionPosition::Scalar(scalar) => Self::ScalarType(scalar),
            TypeDefinitionPosition::Object(object) => Self::ObjectType(object),
            TypeDefinitionPosition::Interface(itf) => Self::InterfaceType(itf),
            TypeDefinitionPosition::Union(union) => Self::UnionType(union),
            TypeDefinitionPosition::Enum(enm) => Self::EnumType(enm),
            TypeDefinitionPosition::InputObject(input_object) => {
                Self::InputObjectType(input_object)
            }
        }
    }
}

impl TryFrom<FieldDefinitionPosition> for DirectiveTargetPosition {
    type Error = PositionConvertError<FieldDefinitionPosition>;

    fn try_from(value: FieldDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            FieldDefinitionPosition::Object(obj_field) => Ok(Self::ObjectField(obj_field)),
            FieldDefinitionPosition::Interface(itf_field) => Ok(Self::InterfaceField(itf_field)),
            // The only field that can occur here is `__typename` on a Union, and meta fields
            // cannot have directives
            FieldDefinitionPosition::Union(_) => Err(PositionConvertError {
                actual: value,
                expected: "DirectiveTargetPosition",
            }),
        }
    }
}

pub(crate) fn is_graphql_reserved_name(name: &str) -> bool {
    name.starts_with("__")
}

pub(crate) static INTROSPECTION_TYPENAME_FIELD_NAME: Name = name!("__typename");

fn validate_component_directives(
    directives: &[Component<Directive>],
) -> Result<(), FederationError> {
    for directive in directives.iter() {
        if directives
            .iter()
            .filter(|other_directive| other_directive.ptr_eq(directive))
            .count()
            > 1
        {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Directive application \"@{}\" is duplicated on schema element",
                    directive.name,
                ),
            }
            .into());
        }
    }
    Ok(())
}

fn validate_node_directives(directives: &[Node<Directive>]) -> Result<(), FederationError> {
    for directive in directives.iter() {
        if directives
            .iter()
            .filter(|other_directive| other_directive.ptr_eq(directive))
            .count()
            > 1
        {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Directive application \"@{}\" is duplicated on schema element",
                    directive.name,
                ),
            }
            .into());
        }
    }
    Ok(())
}

fn validate_arguments(arguments: &[Node<InputValueDefinition>]) -> Result<(), FederationError> {
    for argument in arguments.iter() {
        if arguments
            .iter()
            .filter(|other_argument| other_argument.name == argument.name)
            .count()
            > 1
        {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Argument \"{}\" is duplicated on schema element",
                    argument.name,
                ),
            }
            .into());
        }
    }
    Ok(())
}

fn rename_type(ast_type: &mut ast::Type, new_name: Name) {
    match ast_type {
        ast::Type::Named(name) => *name = new_name,
        ast::Type::NonNullNamed(name) => *name = new_name,
        ast::Type::List(boxed) => rename_type(boxed, new_name),
        ast::Type::NonNullList(boxed) => rename_type(boxed, new_name),
    }
}

impl FederationSchema {
    /// Note that the input schema must be partially valid, in that:
    ///
    /// 1. All schema element references must point to an existing schema element of the appropriate
    ///    kind (e.g. object type fields must return an existing output type).
    /// 2. If the schema uses the core/link spec, then usages of the @core/@link directive must be
    ///    valid.
    ///
    /// The input schema may be otherwise invalid GraphQL (e.g. it may not contain a Query type). If
    /// you want a ValidFederationSchema, use ValidFederationSchema::new() instead.
    pub(crate) fn new(schema: Schema) -> Result<Self, FederationError> {
        let mut schema = Self::new_uninitialized(schema)?;
        schema.collect_links_metadata()?;
        schema.collect_shallow_references();
        schema.collect_deep_references()?;
        Ok(schema)
    }

    pub(crate) fn new_uninitialized(schema: Schema) -> Result<FederationSchema, FederationError> {
        Ok(Self {
            schema,
            referencers: Default::default(),
            links_metadata: None,
            subgraph_metadata: None,
        })
    }

    pub(crate) fn collect_links_metadata(&mut self) -> Result<(), FederationError> {
        self.links_metadata = links_metadata(self.schema())?.map(Box::new);
        Ok(())
    }

    pub(crate) fn collect_shallow_references(&mut self) {
        for (type_name, type_) in self.schema.types.iter() {
            match type_ {
                ExtendedType::Scalar(_) => {
                    self.referencers
                        .scalar_types
                        .insert(type_name.clone(), Default::default());
                }
                ExtendedType::Object(_) => {
                    self.referencers
                        .object_types
                        .insert(type_name.clone(), Default::default());
                }
                ExtendedType::Interface(_) => {
                    self.referencers
                        .interface_types
                        .insert(type_name.clone(), Default::default());
                }
                ExtendedType::Union(_) => {
                    self.referencers
                        .union_types
                        .insert(type_name.clone(), Default::default());
                }
                ExtendedType::Enum(_) => {
                    self.referencers
                        .enum_types
                        .insert(type_name.clone(), Default::default());
                }
                ExtendedType::InputObject(_) => {
                    self.referencers
                        .input_object_types
                        .insert(type_name.clone(), Default::default());
                }
            }
        }

        for directive_name in self.schema.directive_definitions.keys() {
            self.referencers
                .directives
                .insert(directive_name.clone(), Default::default());
        }
    }

    pub(crate) fn collect_deep_references(&mut self) -> Result<(), FederationError> {
        SchemaDefinitionPosition.insert_references(
            &self.schema.schema_definition,
            &self.schema,
            &mut self.referencers,
        )?;
        for (type_name, type_) in self.schema.types.iter() {
            match type_ {
                ExtendedType::Scalar(type_) => {
                    ScalarTypeDefinitionPosition {
                        type_name: type_name.clone(),
                    }
                    .insert_references(type_, &mut self.referencers)?;
                }
                ExtendedType::Object(type_) => {
                    ObjectTypeDefinitionPosition {
                        type_name: type_name.clone(),
                    }
                    .insert_references(
                        type_,
                        &self.schema,
                        &mut self.referencers,
                    )?;
                }
                ExtendedType::Interface(type_) => {
                    InterfaceTypeDefinitionPosition {
                        type_name: type_name.clone(),
                    }
                    .insert_references(
                        type_,
                        &self.schema,
                        &mut self.referencers,
                    )?;
                }
                ExtendedType::Union(type_) => {
                    UnionTypeDefinitionPosition {
                        type_name: type_name.clone(),
                    }
                    .insert_references(type_, &mut self.referencers)?;
                }
                ExtendedType::Enum(type_) => {
                    EnumTypeDefinitionPosition {
                        type_name: type_name.clone(),
                    }
                    .insert_references(type_, &mut self.referencers)?;
                }
                ExtendedType::InputObject(type_) => {
                    InputObjectTypeDefinitionPosition {
                        type_name: type_name.clone(),
                    }
                    .insert_references(type_, &mut self.referencers)?;
                }
            }
        }
        for (directive_name, directive) in self.schema.directive_definitions.iter() {
            DirectiveDefinitionPosition {
                directive_name: directive_name.clone(),
            }
            .insert_references(directive, &mut self.referencers)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_type_recursive() {
        let schema = r#"
            type User {
                id: ID!
                profile: UserProfile
            }
            type UserProfile {
                username: String!
            }
            type Query {
                me: User
            }
        "#;

        let mut schema = FederationSchema::new(
            Schema::parse_and_validate(schema, "schema.graphql")
                .unwrap()
                .into_inner(),
        )
        .unwrap();

        let position = ObjectTypeDefinitionPosition::new(name!("UserProfile"));
        position.remove_recursive(&mut schema).unwrap();

        insta::assert_snapshot!(schema.schema(), @r#"
            type User {
              id: ID!
            }

            type Query {
              me: User
            }
        "#);
    }

    #[test]
    fn remove_interface_recursive() {
        let schema = r#"
            type User {
                id: ID!
                profile: UserProfile
            }
            interface UserProfile {
                username: String!
            }
            type Query {
                me: User
            }
        "#;

        let mut schema = FederationSchema::new(
            Schema::parse_and_validate(schema, "schema.graphql")
                .unwrap()
                .into_inner(),
        )
        .unwrap();

        let position = InterfaceTypeDefinitionPosition {
            type_name: name!("UserProfile"),
        };
        position.remove_recursive(&mut schema).unwrap();

        insta::assert_snapshot!(schema.schema(), @r#"
            type User {
              id: ID!
            }

            type Query {
              me: User
            }
        "#);
    }

    #[test]
    fn rename_type() {
        let schema = Schema::parse_and_validate(
            r#"
            schema {
                query: MyQuery
            }

            type MyQuery {
                a: MyData
            }

            interface OtherInterface {
                b: MyValue
            }

            interface IMyData implements OtherInterface {
                b: MyValue
            }

            type MyData implements IMyData & OtherInterface {
                b: MyValue
                c: String
            }

            type OtherData {
                d: String
                e: MyAorB
            }

            union MyUnionData = MyData | OtherData

            scalar MyValue

            enum MyAorB {
                A
                B
            }
        "#,
            "test-schema.graphqls",
        )
        .unwrap();
        let mut schema = FederationSchema::new(schema.into_inner()).unwrap();

        let query_position = ObjectTypeDefinitionPosition::new(name!("MyQuery"));
        let interface_position = InterfaceTypeDefinitionPosition {
            type_name: name!("IMyData"),
        };
        let data_position = ObjectTypeDefinitionPosition::new(name!("MyData"));
        let scalar_position = ScalarTypeDefinitionPosition {
            type_name: name!("MyValue"),
        };
        let union_position = UnionTypeDefinitionPosition {
            type_name: name!("MyUnionData"),
        };
        let enum_position = EnumTypeDefinitionPosition {
            type_name: name!("MyAorB"),
        };

        query_position.rename(&mut schema, name!("Query")).unwrap();
        interface_position
            .rename(&mut schema, name!("IData"))
            .unwrap();
        data_position.rename(&mut schema, name!("Data")).unwrap();
        scalar_position.rename(&mut schema, name!("Value")).unwrap();
        union_position
            .rename(&mut schema, name!("UnionData"))
            .unwrap();
        enum_position.rename(&mut schema, name!("AorB")).unwrap();

        insta::assert_snapshot!(schema.schema(), @r#"
            schema {
              query: Query
            }

            type Query {
              a: Data
            }

            interface OtherInterface {
              b: Value
            }

            interface IData implements OtherInterface {
              b: Value
            }

            type Data implements OtherInterface & IData {
              b: Value
              c: String
            }

            type OtherData {
              d: String
              e: AorB
            }

            union UnionData = OtherData | Data

            scalar Value

            enum AorB {
              A
              B
            }
        "#);
    }
}
