use std::fmt::Debug;
use std::fmt::Display;
use std::fmt::Formatter;
use std::ops::Deref;

use apollo_compiler::ast;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ComponentName;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::DirectiveDefinition;
use apollo_compiler::schema::EnumType;
use apollo_compiler::schema::EnumValueDefinition;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::FieldDefinition;
use apollo_compiler::schema::InputObjectType;
use apollo_compiler::schema::InputValueDefinition;
use apollo_compiler::schema::InterfaceType;
use apollo_compiler::schema::Name;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::schema::ScalarType;
use apollo_compiler::schema::SchemaDefinition;
use apollo_compiler::schema::UnionType;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use indexmap::IndexSet;
use lazy_static::lazy_static;
use strum::IntoEnumIterator;

use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::link::database::links_metadata;
use crate::link::federation_spec_definition::FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::referencer::DirectiveReferencers;
use crate::schema::referencer::EnumTypeReferencers;
use crate::schema::referencer::InputObjectTypeReferencers;
use crate::schema::referencer::InterfaceTypeReferencers;
use crate::schema::referencer::ObjectTypeReferencers;
use crate::schema::referencer::Referencers;
use crate::schema::referencer::ScalarTypeReferencers;
use crate::schema::referencer::UnionTypeReferencers;
use crate::schema::FederationSchema;

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

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema ExtendedType, FederationError> {
        let ty = schema
            .types
            .get(self.type_name())
            .ok_or_else(|| FederationError::internal(format!(r#"Schema has no type "{self}""#)))?;
        match (ty, self) {
            (ExtendedType::Scalar(_), TypeDefinitionPosition::Scalar(_))
            | (ExtendedType::Object(_), TypeDefinitionPosition::Object(_))
            | (ExtendedType::Interface(_), TypeDefinitionPosition::Interface(_))
            | (ExtendedType::Union(_), TypeDefinitionPosition::Union(_))
            | (ExtendedType::Enum(_), TypeDefinitionPosition::Enum(_))
            | (ExtendedType::InputObject(_), TypeDefinitionPosition::InputObject(_)) => Ok(ty),
            _ => Err(FederationError::internal(format!(
                r#"Schema type "{self}" is the wrong kind"#
            ))),
        }
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema ExtendedType> {
        self.get(schema).ok()
    }
}

impl TryFrom<TypeDefinitionPosition> for ScalarTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: TypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            TypeDefinitionPosition::Scalar(value) => Ok(value),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not a scalar type"#
            ))),
        }
    }
}

impl TryFrom<TypeDefinitionPosition> for ObjectTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: TypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            TypeDefinitionPosition::Object(value) => Ok(value),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an object type"#
            ))),
        }
    }
}

impl TryFrom<TypeDefinitionPosition> for InterfaceTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: TypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            TypeDefinitionPosition::Interface(value) => Ok(value),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an interface type"#
            ))),
        }
    }
}

impl TryFrom<TypeDefinitionPosition> for UnionTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: TypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            TypeDefinitionPosition::Union(value) => Ok(value),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not a union type"#
            ))),
        }
    }
}

impl TryFrom<TypeDefinitionPosition> for EnumTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: TypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            TypeDefinitionPosition::Enum(value) => Ok(value),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an enum type"#
            ))),
        }
    }
}

impl TryFrom<TypeDefinitionPosition> for InputObjectTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: TypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            TypeDefinitionPosition::InputObject(value) => Ok(value),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an input object type"#
            ))),
        }
    }
}

impl From<OutputTypeDefinitionPosition> for TypeDefinitionPosition {
    fn from(value: OutputTypeDefinitionPosition) -> Self {
        match value {
            OutputTypeDefinitionPosition::Scalar(value) => value.into(),
            OutputTypeDefinitionPosition::Object(value) => value.into(),
            OutputTypeDefinitionPosition::Interface(value) => value.into(),
            OutputTypeDefinitionPosition::Union(value) => value.into(),
            OutputTypeDefinitionPosition::Enum(value) => value.into(),
        }
    }
}

impl From<CompositeTypeDefinitionPosition> for TypeDefinitionPosition {
    fn from(value: CompositeTypeDefinitionPosition) -> Self {
        match value {
            CompositeTypeDefinitionPosition::Object(value) => value.into(),
            CompositeTypeDefinitionPosition::Interface(value) => value.into(),
            CompositeTypeDefinitionPosition::Union(value) => value.into(),
        }
    }
}

impl From<AbstractTypeDefinitionPosition> for TypeDefinitionPosition {
    fn from(value: AbstractTypeDefinitionPosition) -> Self {
        match value {
            AbstractTypeDefinitionPosition::Interface(value) => value.into(),
            AbstractTypeDefinitionPosition::Union(value) => value.into(),
        }
    }
}

impl From<ObjectOrInterfaceTypeDefinitionPosition> for TypeDefinitionPosition {
    fn from(value: ObjectOrInterfaceTypeDefinitionPosition) -> Self {
        match value {
            ObjectOrInterfaceTypeDefinitionPosition::Object(value) => value.into(),
            ObjectOrInterfaceTypeDefinitionPosition::Interface(value) => value.into(),
        }
    }
}

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
    pub(crate) fn type_name(&self) -> &Name {
        match self {
            OutputTypeDefinitionPosition::Scalar(type_) => &type_.type_name,
            OutputTypeDefinitionPosition::Object(type_) => &type_.type_name,
            OutputTypeDefinitionPosition::Interface(type_) => &type_.type_name,
            OutputTypeDefinitionPosition::Union(type_) => &type_.type_name,
            OutputTypeDefinitionPosition::Enum(type_) => &type_.type_name,
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema ExtendedType, FederationError> {
        let ty = schema
            .types
            .get(self.type_name())
            .ok_or_else(|| FederationError::internal(format!(r#"Schema has no type "{self}""#)))?;
        match (ty, self) {
            (ExtendedType::Scalar(_), OutputTypeDefinitionPosition::Scalar(_))
            | (ExtendedType::Object(_), OutputTypeDefinitionPosition::Object(_))
            | (ExtendedType::Interface(_), OutputTypeDefinitionPosition::Interface(_))
            | (ExtendedType::Union(_), OutputTypeDefinitionPosition::Union(_))
            | (ExtendedType::Enum(_), OutputTypeDefinitionPosition::Enum(_)) => Ok(ty),
            _ => Err(FederationError::internal(format!(
                r#"Schema type "{self}" is the wrong kind"#
            ))),
        }
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema ExtendedType> {
        self.get(schema).ok()
    }
}

impl TryFrom<OutputTypeDefinitionPosition> for ScalarTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: OutputTypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            OutputTypeDefinitionPosition::Scalar(value) => Ok(value),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not a scalar type"#
            ))),
        }
    }
}

impl TryFrom<OutputTypeDefinitionPosition> for ObjectTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: OutputTypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            OutputTypeDefinitionPosition::Object(value) => Ok(value),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an object type"#
            ))),
        }
    }
}

impl TryFrom<OutputTypeDefinitionPosition> for InterfaceTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: OutputTypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            OutputTypeDefinitionPosition::Interface(value) => Ok(value),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an interface type"#
            ))),
        }
    }
}

impl TryFrom<OutputTypeDefinitionPosition> for UnionTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: OutputTypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            OutputTypeDefinitionPosition::Union(value) => Ok(value),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not a union type"#
            ))),
        }
    }
}

impl TryFrom<OutputTypeDefinitionPosition> for EnumTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: OutputTypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            OutputTypeDefinitionPosition::Enum(value) => Ok(value),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an enum type"#
            ))),
        }
    }
}

impl TryFrom<TypeDefinitionPosition> for OutputTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: TypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            TypeDefinitionPosition::Scalar(value) => Ok(value.into()),
            TypeDefinitionPosition::Object(value) => Ok(value.into()),
            TypeDefinitionPosition::Interface(value) => Ok(value.into()),
            TypeDefinitionPosition::Enum(value) => Ok(value.into()),
            TypeDefinitionPosition::Union(value) => Ok(value.into()),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an output type"#
            ))),
        }
    }
}

impl From<CompositeTypeDefinitionPosition> for OutputTypeDefinitionPosition {
    fn from(value: CompositeTypeDefinitionPosition) -> Self {
        match value {
            CompositeTypeDefinitionPosition::Object(value) => value.into(),
            CompositeTypeDefinitionPosition::Interface(value) => value.into(),
            CompositeTypeDefinitionPosition::Union(value) => value.into(),
        }
    }
}

impl From<AbstractTypeDefinitionPosition> for OutputTypeDefinitionPosition {
    fn from(value: AbstractTypeDefinitionPosition) -> Self {
        match value {
            AbstractTypeDefinitionPosition::Interface(value) => value.into(),
            AbstractTypeDefinitionPosition::Union(value) => value.into(),
        }
    }
}

impl From<ObjectOrInterfaceTypeDefinitionPosition> for OutputTypeDefinitionPosition {
    fn from(value: ObjectOrInterfaceTypeDefinitionPosition) -> Self {
        match value {
            ObjectOrInterfaceTypeDefinitionPosition::Object(value) => value.into(),
            ObjectOrInterfaceTypeDefinitionPosition::Interface(value) => value.into(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, derive_more::From, derive_more::Display)]
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
    ) -> Result<&'schema ExtendedType, FederationError> {
        let ty = schema
            .types
            .get(self.type_name())
            .ok_or_else(|| FederationError::internal(format!(r#"Schema has no type "{self}""#)))?;
        match (ty, self) {
            (ExtendedType::Object(_), CompositeTypeDefinitionPosition::Object(_))
            | (ExtendedType::Interface(_), CompositeTypeDefinitionPosition::Interface(_))
            | (ExtendedType::Union(_), CompositeTypeDefinitionPosition::Union(_)) => Ok(ty),
            _ => Err(FederationError::internal(format!(
                r#"Schema type "{self}" is the wrong kind"#
            ))),
        }
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema ExtendedType> {
        self.get(schema).ok()
    }

    pub(crate) fn is_interface_object_type(&self, schema: &Schema) -> bool {
        let Ok(ExtendedType::Object(obj_type_def)) = self.get(schema) else {
            return false;
        };
        obj_type_def
            .directives
            .has(FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC.as_str())
    }
}

impl TryFrom<CompositeTypeDefinitionPosition> for ObjectTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: CompositeTypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            CompositeTypeDefinitionPosition::Object(value) => Ok(value),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an object type"#
            ))),
        }
    }
}

impl TryFrom<CompositeTypeDefinitionPosition> for InterfaceTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: CompositeTypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            CompositeTypeDefinitionPosition::Interface(value) => Ok(value),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an interface type"#
            ))),
        }
    }
}

impl TryFrom<CompositeTypeDefinitionPosition> for UnionTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: CompositeTypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            CompositeTypeDefinitionPosition::Union(value) => Ok(value),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not a union type"#
            ))),
        }
    }
}

impl TryFrom<TypeDefinitionPosition> for CompositeTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: TypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            TypeDefinitionPosition::Object(value) => Ok(value.into()),
            TypeDefinitionPosition::Interface(value) => Ok(value.into()),
            TypeDefinitionPosition::Union(value) => Ok(value.into()),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not a composite type"#
            ))),
        }
    }
}

impl TryFrom<OutputTypeDefinitionPosition> for CompositeTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: OutputTypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            OutputTypeDefinitionPosition::Object(value) => Ok(value.into()),
            OutputTypeDefinitionPosition::Interface(value) => Ok(value.into()),
            OutputTypeDefinitionPosition::Union(value) => Ok(value.into()),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not a composite type"#
            ))),
        }
    }
}

impl From<AbstractTypeDefinitionPosition> for CompositeTypeDefinitionPosition {
    fn from(value: AbstractTypeDefinitionPosition) -> Self {
        match value {
            AbstractTypeDefinitionPosition::Interface(value) => value.into(),
            AbstractTypeDefinitionPosition::Union(value) => value.into(),
        }
    }
}

impl From<ObjectOrInterfaceTypeDefinitionPosition> for CompositeTypeDefinitionPosition {
    fn from(value: ObjectOrInterfaceTypeDefinitionPosition) -> Self {
        match value {
            ObjectOrInterfaceTypeDefinitionPosition::Object(value) => value.into(),
            ObjectOrInterfaceTypeDefinitionPosition::Interface(value) => value.into(),
        }
    }
}

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
    pub(crate) fn type_name(&self) -> &Name {
        match self {
            AbstractTypeDefinitionPosition::Interface(type_) => &type_.type_name,
            AbstractTypeDefinitionPosition::Union(type_) => &type_.type_name,
        }
    }

    pub(crate) fn field(
        &self,
        field_name: Name,
    ) -> Result<FieldDefinitionPosition, FederationError> {
        match self {
            AbstractTypeDefinitionPosition::Interface(type_) => Ok(type_.field(field_name).into()),
            AbstractTypeDefinitionPosition::Union(type_) => {
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
            AbstractTypeDefinitionPosition::Interface(type_) => {
                type_.introspection_typename_field().into()
            }
            AbstractTypeDefinitionPosition::Union(type_) => {
                type_.introspection_typename_field().into()
            }
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema ExtendedType, FederationError> {
        let ty = schema
            .types
            .get(self.type_name())
            .ok_or_else(|| FederationError::internal(format!(r#"Schema has no type "{self}""#)))?;
        match (ty, self) {
            (ExtendedType::Interface(_), AbstractTypeDefinitionPosition::Interface(_))
            | (ExtendedType::Union(_), AbstractTypeDefinitionPosition::Union(_)) => Ok(ty),
            _ => Err(FederationError::internal(format!(
                r#"Schema type "{self}" is the wrong kind"#
            ))),
        }
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema ExtendedType> {
        self.get(schema).ok()
    }
}

impl TryFrom<AbstractTypeDefinitionPosition> for InterfaceTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: AbstractTypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            AbstractTypeDefinitionPosition::Interface(value) => Ok(value),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an interface type"#
            ))),
        }
    }
}

impl TryFrom<AbstractTypeDefinitionPosition> for UnionTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: AbstractTypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            AbstractTypeDefinitionPosition::Union(value) => Ok(value),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not a union type"#
            ))),
        }
    }
}

impl TryFrom<TypeDefinitionPosition> for AbstractTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: TypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            TypeDefinitionPosition::Interface(value) => Ok(value.into()),
            TypeDefinitionPosition::Union(value) => Ok(value.into()),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an abstract type"#
            ))),
        }
    }
}

impl TryFrom<OutputTypeDefinitionPosition> for AbstractTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: OutputTypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            OutputTypeDefinitionPosition::Interface(value) => Ok(value.into()),
            OutputTypeDefinitionPosition::Union(value) => Ok(value.into()),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an abstract type"#
            ))),
        }
    }
}

impl TryFrom<CompositeTypeDefinitionPosition> for AbstractTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: CompositeTypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            CompositeTypeDefinitionPosition::Interface(value) => Ok(value.into()),
            CompositeTypeDefinitionPosition::Union(value) => Ok(value.into()),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an abstract type"#
            ))),
        }
    }
}

impl TryFrom<ObjectOrInterfaceTypeDefinitionPosition> for AbstractTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: ObjectOrInterfaceTypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            ObjectOrInterfaceTypeDefinitionPosition::Interface(value) => Ok(value.into()),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an abstract type"#
            ))),
        }
    }
}

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

    pub(crate) fn introspection_typename_field(&self) -> FieldDefinitionPosition {
        match self {
            ObjectOrInterfaceTypeDefinitionPosition::Object(type_) => {
                type_.introspection_typename_field().into()
            }
            ObjectOrInterfaceTypeDefinitionPosition::Interface(type_) => {
                type_.introspection_typename_field().into()
            }
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema ExtendedType, FederationError> {
        let ty = schema
            .types
            .get(self.type_name())
            .ok_or_else(|| FederationError::internal(format!(r#"Schema has no type "{self}""#)))?;
        match (ty, self) {
            (ExtendedType::Object(_), ObjectOrInterfaceTypeDefinitionPosition::Object(_))
            | (ExtendedType::Interface(_), ObjectOrInterfaceTypeDefinitionPosition::Interface(_)) => {
                Ok(ty)
            }
            _ => Err(FederationError::internal(format!(
                r#"Schema type "{self}" is the wrong kind"#
            ))),
        }
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema ExtendedType> {
        self.get(schema).ok()
    }
}

impl TryFrom<ObjectOrInterfaceTypeDefinitionPosition> for ObjectTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: ObjectOrInterfaceTypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            ObjectOrInterfaceTypeDefinitionPosition::Object(value) => Ok(value),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an object type"#
            ))),
        }
    }
}

impl TryFrom<ObjectOrInterfaceTypeDefinitionPosition> for InterfaceTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: ObjectOrInterfaceTypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            ObjectOrInterfaceTypeDefinitionPosition::Interface(value) => Ok(value),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an interface type"#
            ))),
        }
    }
}

impl TryFrom<TypeDefinitionPosition> for ObjectOrInterfaceTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: TypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            TypeDefinitionPosition::Object(value) => Ok(value.into()),
            TypeDefinitionPosition::Interface(value) => Ok(value.into()),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an object/interface type"#
            ))),
        }
    }
}

impl TryFrom<OutputTypeDefinitionPosition> for ObjectOrInterfaceTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: OutputTypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            OutputTypeDefinitionPosition::Object(value) => Ok(value.into()),
            OutputTypeDefinitionPosition::Interface(value) => Ok(value.into()),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an object/interface type"#
            ))),
        }
    }
}

impl TryFrom<CompositeTypeDefinitionPosition> for ObjectOrInterfaceTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: CompositeTypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            CompositeTypeDefinitionPosition::Object(value) => Ok(value.into()),
            CompositeTypeDefinitionPosition::Interface(value) => Ok(value.into()),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an object/interface type"#
            ))),
        }
    }
}

impl TryFrom<AbstractTypeDefinitionPosition> for ObjectOrInterfaceTypeDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: AbstractTypeDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            AbstractTypeDefinitionPosition::Interface(value) => Ok(value.into()),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an object/interface type"#
            ))),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, derive_more::From, derive_more::Display)]
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

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Component<FieldDefinition>, FederationError> {
        match self {
            FieldDefinitionPosition::Object(field) => field.get(schema),
            FieldDefinitionPosition::Interface(field) => field.get(schema),
            FieldDefinitionPosition::Union(field) => field.get(schema),
        }
    }
}

impl From<ObjectOrInterfaceFieldDefinitionPosition> for FieldDefinitionPosition {
    fn from(value: ObjectOrInterfaceFieldDefinitionPosition) -> Self {
        match value {
            ObjectOrInterfaceFieldDefinitionPosition::Object(value) => value.into(),
            ObjectOrInterfaceFieldDefinitionPosition::Interface(value) => value.into(),
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

    pub(crate) fn is_introspection_typename_field(&self) -> bool {
        *self.field_name() == *INTROSPECTION_TYPENAME_FIELD_NAME
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
    ) -> Result<&'schema Component<FieldDefinition>, FederationError> {
        match self {
            ObjectOrInterfaceFieldDefinitionPosition::Object(field) => field.get(schema),
            ObjectOrInterfaceFieldDefinitionPosition::Interface(field) => field.get(schema),
        }
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
}

impl TryFrom<FieldDefinitionPosition> for ObjectOrInterfaceFieldDefinitionPosition {
    type Error = FederationError;

    fn try_from(value: FieldDefinitionPosition) -> Result<Self, Self::Error> {
        match value {
            FieldDefinitionPosition::Object(value) => Ok(value.into()),
            FieldDefinitionPosition::Interface(value) => Ok(value.into()),
            _ => Err(FederationError::internal(format!(
                r#"Type "{value}" was unexpectedly not an object/interface field"#
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SchemaDefinitionPosition;

impl SchemaDefinitionPosition {
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
        schema_definition.make_mut().directives.push(directive);
        self.insert_directive_name_references(&mut schema.referencers, &name)?;
        schema.links_metadata = links_metadata(&schema.schema)?.map(Box::new);
        Ok(())
    }

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

    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Component<Directive>,
    ) -> Result<(), FederationError> {
        let is_link = Self::is_link(schema, &directive.name)?;
        let schema_definition = self.make_mut(&mut schema.schema);
        if !schema_definition.directives.iter().any(|other_directive| {
            (other_directive.name == directive.name) && !other_directive.ptr_eq(directive)
        }) {
            self.remove_directive_name_references(&mut schema.referencers, &directive.name);
        }
        schema_definition
            .make_mut()
            .directives
            .retain(|other_directive| !other_directive.ptr_eq(directive));
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
                    "Schema definition's directive application \"@{}\" does not refer to an existing directive.",
                    name,
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

#[derive(
    Debug, Copy, Clone, PartialEq, Eq, Hash, strum_macros::Display, strum_macros::EnumIter,
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
                    message: format!("Schema definition has no root {} type", self),
                }
                .into()
            }),
            SchemaRootDefinitionKind::Mutation => {
                schema_definition.mutation.as_ref().ok_or_else(|| {
                    SingleFederationError::Internal {
                        message: format!("Schema definition has no root {} type", self),
                    }
                    .into()
                })
            }
            SchemaRootDefinitionKind::Subscription => {
                schema_definition.subscription.as_ref().ok_or_else(|| {
                    SingleFederationError::Internal {
                        message: format!("Schema definition has no root {} type", self),
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

    fn make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Result<&'schema mut ComponentName, FederationError> {
        let schema_definition = self.parent().make_mut(schema).make_mut();

        match self.root_kind {
            SchemaRootDefinitionKind::Query => schema_definition.query.as_mut().ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Schema definition has no root {} type", self),
                }
                .into()
            }),
            SchemaRootDefinitionKind::Mutation => {
                schema_definition.mutation.as_mut().ok_or_else(|| {
                    SingleFederationError::Internal {
                        message: format!("Schema definition has no root {} type", self),
                    }
                    .into()
                })
            }
            SchemaRootDefinitionKind::Subscription => {
                schema_definition.subscription.as_mut().ok_or_else(|| {
                    SingleFederationError::Internal {
                        message: format!("Schema definition has no root {} type", self),
                    }
                    .into()
                })
            }
        }
    }

    fn try_make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Option<&'schema mut ComponentName> {
        if self.try_get(schema).is_some() {
            self.make_mut(schema).ok()
        } else {
            None
        }
    }

    pub(crate) fn insert(
        &self,
        schema: &mut FederationSchema,
        root_type: ComponentName,
    ) -> Result<(), FederationError> {
        if self.try_get(&schema.schema).is_some() {
            return Err(SingleFederationError::Internal {
                message: format!("Root {} already exists on schema definition", self),
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
    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<ScalarType>, FederationError> {
        schema
            .types
            .get(&self.type_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Schema has no type \"{}\"", self),
                }
                .into()
            })
            .and_then(|type_| {
                if let ExtendedType::Scalar(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(SingleFederationError::Internal {
                        message: format!("Schema type \"{}\" was not a scalar", self),
                    }
                    .into())
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
    ) -> Result<&'schema mut Node<ScalarType>, FederationError> {
        schema
            .types
            .get_mut(&self.type_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Schema has no type \"{}\"", self),
                }
                .into()
            })
            .and_then(|type_| {
                if let ExtendedType::Scalar(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(SingleFederationError::Internal {
                        message: format!("Schema type \"{}\" was not a scalar", self),
                    }
                    .into())
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
            // TODO: Allow built-in shadowing instead of ignoring them
            if is_graphql_reserved_name(&self.type_name)
                || GRAPHQL_BUILTIN_SCALAR_NAMES.contains(&self.type_name)
            {
                return Ok(());
            }
            return Err(SingleFederationError::Internal {
                message: format!("Type \"{}\" has already been pre-inserted", self),
            }
            .into());
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
            return Err(SingleFederationError::Internal {
                message: format!("Type \"{}\" has not been pre-inserted", self),
            }
            .into());
        }
        if schema.schema.types.contains_key(&self.type_name) {
            // TODO: Allow built-in shadowing instead of ignoring them
            if is_graphql_reserved_name(&self.type_name)
                || GRAPHQL_BUILTIN_SCALAR_NAMES.contains(&self.type_name)
            {
                return Ok(());
            }
            return Err(SingleFederationError::Internal {
                message: format!("Type \"{}\" already exists in schema", self),
            }
            .into());
        }
        schema
            .schema
            .types
            .insert(self.type_name.clone(), ExtendedType::Scalar(type_));
        self.insert_references(self.get(&schema.schema)?, &mut schema.referencers)
    }

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
        for argument in referencers.object_field_arguments {
            argument.remove(schema)?;
        }
        for field in referencers.interface_fields {
            field.remove_recursive(schema)?;
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
                    message: format!("Schema missing referencers for type \"{}\"", self),
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

    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Component<Directive>,
    ) {
        let Some(type_) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        if !type_.directives.iter().any(|other_directive| {
            (other_directive.name == directive.name) && !other_directive.ptr_eq(directive)
        }) {
            self.remove_directive_name_references(&mut schema.referencers, &directive.name);
        }
        type_
            .make_mut()
            .directives
            .retain(|other_directive| !other_directive.ptr_eq(directive));
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
                    "Scalar type \"{}\"'s directive application \"@{}\" does not refer to an existing directive.",
                    self,
                    name,
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
}

impl Display for ScalarTypeDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.type_name)
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct ObjectTypeDefinitionPosition {
    pub(crate) type_name: Name,
}

impl ObjectTypeDefinitionPosition {
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

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<ObjectType>, FederationError> {
        schema
            .types
            .get(&self.type_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Schema has no type \"{}\"", self),
                }
                .into()
            })
            .and_then(|type_| {
                if let ExtendedType::Object(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(SingleFederationError::Internal {
                        message: format!("Schema type \"{}\" was not an object", self),
                    }
                    .into())
                }
            })
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema Node<ObjectType>> {
        self.get(schema).ok()
    }

    fn make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Result<&'schema mut Node<ObjectType>, FederationError> {
        schema
            .types
            .get_mut(&self.type_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Schema has no type \"{}\"", self),
                }
                .into()
            })
            .and_then(|type_| {
                if let ExtendedType::Object(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(SingleFederationError::Internal {
                        message: format!("Schema type \"{}\" was not an object", self),
                    }
                    .into())
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
            // TODO: Allow built-in shadowing instead of ignoring them
            if is_graphql_reserved_name(&self.type_name) {
                return Ok(());
            }
            return Err(SingleFederationError::Internal {
                message: format!("Type \"{}\" has already been pre-inserted", self),
            }
            .into());
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
            return Err(SingleFederationError::Internal {
                message: format!("Type \"{}\" has not been pre-inserted", self),
            }
            .into());
        }
        if schema.schema.types.contains_key(&self.type_name) {
            // TODO: Allow built-in shadowing instead of ignoring them
            if is_graphql_reserved_name(&self.type_name) {
                return Ok(());
            }
            return Err(SingleFederationError::Internal {
                message: format!("Type \"{}\" already exists in schema", self),
            }
            .into());
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
                    message: format!("Schema missing referencers for type \"{}\"", self),
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

    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Component<Directive>,
    ) {
        let Some(type_) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        if !type_.directives.iter().any(|other_directive| {
            (other_directive.name == directive.name) && !other_directive.ptr_eq(directive)
        }) {
            self.remove_directive_name_references(&mut schema.referencers, &directive.name);
        }
        type_
            .make_mut()
            .directives
            .retain(|other_directive| !other_directive.ptr_eq(directive));
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
            schema,
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
                .insert_references(field, schema, referencers, false)?;
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
            schema,
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
                .remove_references(field, schema, referencers, false)?;
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
                    "Object type \"{}\"'s directive application \"@{}\" does not refer to an existing directive.",
                    self,
                    name,
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
                    "Object type \"{}\"'s implements \"{}\" does not refer to an existing interface.",
                    self,
                    name,
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
            introspection_schema_field.insert_references(field, schema, referencers, true)?;
        }
        let introspection_type_field = self.introspection_type_field();
        if let Some(field) = introspection_type_field.try_get(schema) {
            introspection_type_field.insert_references(field, schema, referencers, true)?;
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
            introspection_schema_field.remove_references(field, schema, referencers, true)?;
        }
        let introspection_type_field = self.introspection_type_field();
        if let Some(field) = introspection_type_field.try_get(schema) {
            introspection_type_field.remove_references(field, schema, referencers, true)?;
        }
        Ok(())
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

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct ObjectFieldDefinitionPosition {
    pub(crate) type_name: Name,
    pub(crate) field_name: Name,
}

impl ObjectFieldDefinitionPosition {
    pub(crate) fn is_introspection_typename_field(&self) -> bool {
        self.field_name == *INTROSPECTION_TYPENAME_FIELD_NAME
    }

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
    ) -> Result<&'schema Component<FieldDefinition>, FederationError> {
        let parent = self.parent();
        parent.get(schema)?;

        schema
            .type_field(&self.type_name, &self.field_name)
            .map_err(|_| {
                SingleFederationError::Internal {
                    message: format!(
                        "Object type \"{}\" has no field \"{}\"",
                        parent, self.field_name
                    ),
                }
                .into()
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
    ) -> Result<&'schema mut Component<FieldDefinition>, FederationError> {
        let parent = self.parent();
        let type_ = parent.make_mut(schema)?.make_mut();

        if is_graphql_reserved_name(&self.field_name) {
            return Err(SingleFederationError::Internal {
                message: format!("Cannot mutate reserved object field \"{}\"", self),
            }
            .into());
        }
        type_.fields.get_mut(&self.field_name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Object type \"{}\" has no field \"{}\"",
                    parent, self.field_name
                ),
            }
            .into()
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
            return Err(SingleFederationError::Internal {
                message: format!("Object field \"{}\" already exists in schema", self),
            }
            .into());
        }
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .fields
            .insert(self.field_name.clone(), field);
        self.insert_references(
            self.get(&schema.schema)?,
            &schema.schema,
            &mut schema.referencers,
            false,
        )?;
        Ok(())
    }

    pub(crate) fn remove(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let Some(field) = self.try_get(&schema.schema) else {
            return Ok(());
        };
        self.remove_references(field, &schema.schema, &mut schema.referencers, false)?;
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .fields
            .shift_remove(&self.field_name);
        Ok(())
    }

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
        schema: &Schema,
        referencers: &mut Referencers,
        allow_built_ins: bool,
    ) -> Result<(), FederationError> {
        if !allow_built_ins && is_graphql_reserved_name(&self.field_name) {
            return Err(SingleFederationError::Internal {
                message: format!("Cannot insert reserved object field \"{}\"", self),
            }
            .into());
        }
        validate_node_directives(field.directives.deref())?;
        for directive_reference in field.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        self.insert_type_references(field, schema, referencers)?;
        validate_arguments(&field.arguments)?;
        for argument in field.arguments.iter() {
            self.argument(argument.name.clone()).insert_references(
                argument,
                schema,
                referencers,
            )?;
        }
        Ok(())
    }

    fn remove_references(
        &self,
        field: &Component<FieldDefinition>,
        schema: &Schema,
        referencers: &mut Referencers,
        allow_built_ins: bool,
    ) -> Result<(), FederationError> {
        if !allow_built_ins && is_graphql_reserved_name(&self.field_name) {
            return Err(SingleFederationError::Internal {
                message: format!("Cannot remove reserved object field \"{}\"", self),
            }
            .into());
        }
        for directive_reference in field.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
        self.remove_type_references(field, schema, referencers)?;
        for argument in field.arguments.iter() {
            self.argument(argument.name.clone()).remove_references(
                argument,
                schema,
                referencers,
            )?;
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
                    "Object field \"{}\"'s directive application \"@{}\" does not refer to an existing directive.",
                    self,
                    name,
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
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let output_type_reference = field.ty.inner_named_type();
        match schema.types.get(output_type_reference) {
            Some(ExtendedType::Scalar(_)) => {
                let scalar_type_referencers = referencers
                    .scalar_types
                    .get_mut(output_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            output_type_reference
                        ),
                    })?;
                scalar_type_referencers.object_fields.insert(self.clone());
            }
            Some(ExtendedType::Object(_)) => {
                let object_type_referencers = referencers
                    .object_types
                    .get_mut(output_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            output_type_reference
                        ),
                    })?;
                object_type_referencers.object_fields.insert(self.clone());
            }
            Some(ExtendedType::Interface(_)) => {
                let interface_type_referencers = referencers
                    .interface_types
                    .get_mut(output_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            output_type_reference
                        ),
                    })?;
                interface_type_referencers
                    .object_fields
                    .insert(self.clone());
            }
            Some(ExtendedType::Union(_)) => {
                let union_type_referencers = referencers
                    .union_types
                    .get_mut(output_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            output_type_reference
                        ),
                    })?;
                union_type_referencers.object_fields.insert(self.clone());
            }
            Some(ExtendedType::Enum(_)) => {
                let enum_type_referencers = referencers
                    .enum_types
                    .get_mut(output_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                    message: format!(
                        "Schema missing referencers for type \"{}\"",
                        output_type_reference
                    ),
                })?;
                enum_type_referencers.object_fields.insert(self.clone());
            }
            _ => {
                return Err(
                    SingleFederationError::Internal {
                        message: format!(
                            "Object field \"{}\"'s inner type \"{}\" does not refer to an existing output type.",
                            self,
                            output_type_reference.deref(),
                        )
                    }.into()
                );
            }
        }
        Ok(())
    }

    fn remove_type_references(
        &self,
        field: &Component<FieldDefinition>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let output_type_reference = field.ty.inner_named_type();
        match schema.types.get(output_type_reference) {
            Some(ExtendedType::Scalar(_)) => {
                let Some(scalar_type_referencers) =
                    referencers.scalar_types.get_mut(output_type_reference)
                else {
                    return Ok(());
                };
                scalar_type_referencers.object_fields.shift_remove(self);
            }
            Some(ExtendedType::Object(_)) => {
                let Some(object_type_referencers) =
                    referencers.object_types.get_mut(output_type_reference)
                else {
                    return Ok(());
                };
                object_type_referencers.object_fields.shift_remove(self);
            }
            Some(ExtendedType::Interface(_)) => {
                let Some(interface_type_referencers) =
                    referencers.interface_types.get_mut(output_type_reference)
                else {
                    return Ok(());
                };
                interface_type_referencers.object_fields.shift_remove(self);
            }
            Some(ExtendedType::Union(_)) => {
                let Some(union_type_referencers) =
                    referencers.union_types.get_mut(output_type_reference)
                else {
                    return Ok(());
                };
                union_type_referencers.object_fields.shift_remove(self);
            }
            Some(ExtendedType::Enum(_)) => {
                let Some(enum_type_referencers) =
                    referencers.enum_types.get_mut(output_type_reference)
                else {
                    return Ok(());
                };
                enum_type_referencers.object_fields.shift_remove(self);
            }
            _ => {
                return Err(
                    SingleFederationError::Internal {
                        message: format!(
                            "Object field \"{}\"'s inner type \"{}\" does not refer to an existing output type.",
                            self,
                            output_type_reference.deref(),
                        )
                    }.into()
                );
            }
        }
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

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct ObjectFieldArgumentDefinitionPosition {
    pub(crate) type_name: Name,
    pub(crate) field_name: Name,
    pub(crate) argument_name: Name,
}

impl ObjectFieldArgumentDefinitionPosition {
    pub(crate) fn parent(&self) -> ObjectFieldDefinitionPosition {
        ObjectFieldDefinitionPosition {
            type_name: self.type_name.clone(),
            field_name: self.field_name.clone(),
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<InputValueDefinition>, FederationError> {
        let parent = self.parent();
        let type_ = parent.get(schema)?;

        type_
            .arguments
            .iter()
            .find(|a| a.name == self.argument_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!(
                        "Object field \"{}\" has no argument \"{}\"",
                        parent, self.argument_name
                    ),
                }
                .into()
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
    ) -> Result<&'schema mut Node<InputValueDefinition>, FederationError> {
        let parent = self.parent();
        let type_ = parent.make_mut(schema)?.make_mut();

        type_
            .arguments
            .iter_mut()
            .find(|a| a.name == self.argument_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!(
                        "Object field \"{}\" has no argument \"{}\"",
                        parent, self.argument_name
                    ),
                }
                .into()
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

    pub(crate) fn insert(
        &self,
        schema: &mut FederationSchema,
        argument: Node<InputValueDefinition>,
    ) -> Result<(), FederationError> {
        if self.argument_name != argument.name {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Object field argument \"{}\" given argument named \"{}\"",
                    self, argument.name,
                ),
            }
            .into());
        }
        if self.try_get(&schema.schema).is_some() {
            // TODO: Handle old spec edge case of arguments with non-unique names
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Object field argument \"{}\" already exists in schema",
                    self,
                ),
            }
            .into());
        }
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .arguments
            .push(argument);
        self.insert_references(
            self.get(&schema.schema)?,
            &schema.schema,
            &mut schema.referencers,
        )
    }

    pub(crate) fn remove(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let Some(argument) = self.try_get(&schema.schema) else {
            return Ok(());
        };
        self.remove_references(argument, &schema.schema, &mut schema.referencers)?;
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

    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Node<Directive>,
    ) {
        let Some(argument) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        if !argument.directives.iter().any(|other_directive| {
            (other_directive.name == directive.name) && !other_directive.ptr_eq(directive)
        }) {
            self.remove_directive_name_references(&mut schema.referencers, &directive.name);
        }
        argument
            .make_mut()
            .directives
            .retain(|other_directive| !other_directive.ptr_eq(directive));
    }

    fn insert_references(
        &self,
        argument: &Node<InputValueDefinition>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if is_graphql_reserved_name(&self.argument_name) {
            return Err(SingleFederationError::Internal {
                message: format!("Cannot insert reserved object field argument \"{}\"", self),
            }
            .into());
        }
        validate_node_directives(argument.directives.deref())?;
        for directive_reference in argument.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        self.insert_type_references(argument, schema, referencers)
    }

    fn remove_references(
        &self,
        argument: &Node<InputValueDefinition>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if is_graphql_reserved_name(&self.argument_name) {
            return Err(SingleFederationError::Internal {
                message: format!("Cannot remove reserved object field argument \"{}\"", self),
            }
            .into());
        }
        for directive_reference in argument.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
        self.remove_type_references(argument, schema, referencers)
    }

    fn insert_directive_name_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let directive_referencers = referencers.directives.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Object field argument \"{}\"'s directive application \"@{}\" does not refer to an existing directive.",
                    self,
                    name,
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
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let input_type_reference = argument.ty.inner_named_type();
        match schema.types.get(input_type_reference) {
            Some(ExtendedType::Scalar(_)) => {
                let scalar_type_referencers = referencers
                    .scalar_types
                    .get_mut(input_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            input_type_reference
                        ),
                    })?;
                scalar_type_referencers
                    .object_field_arguments
                    .insert(self.clone());
            }
            Some(ExtendedType::Enum(_)) => {
                let enum_type_referencers = referencers
                    .enum_types
                    .get_mut(input_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            input_type_reference
                        ),
                    })?;
                enum_type_referencers
                    .object_field_arguments
                    .insert(self.clone());
            }
            Some(ExtendedType::InputObject(_)) => {
                let input_object_type_referencers = referencers
                    .input_object_types
                    .get_mut(input_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            input_type_reference
                        ),
                    })?;
                input_object_type_referencers
                    .object_field_arguments
                    .insert(self.clone());
            }
            _ => {
                return Err(
                    SingleFederationError::Internal {
                        message: format!(
                            "Object field argument \"{}\"'s inner type \"{}\" does not refer to an existing input type.",
                            self,
                            input_type_reference.deref(),
                        )
                    }.into()
                );
            }
        }
        Ok(())
    }

    fn remove_type_references(
        &self,
        argument: &Node<InputValueDefinition>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let input_type_reference = argument.ty.inner_named_type();
        match schema.types.get(input_type_reference) {
            Some(ExtendedType::Scalar(_)) => {
                let Some(scalar_type_referencers) =
                    referencers.scalar_types.get_mut(input_type_reference)
                else {
                    return Ok(());
                };
                scalar_type_referencers
                    .object_field_arguments
                    .shift_remove(self);
            }
            Some(ExtendedType::Enum(_)) => {
                let Some(enum_type_referencers) =
                    referencers.enum_types.get_mut(input_type_reference)
                else {
                    return Ok(());
                };
                enum_type_referencers
                    .object_field_arguments
                    .shift_remove(self);
            }
            Some(ExtendedType::InputObject(_)) => {
                let Some(input_object_type_referencers) =
                    referencers.input_object_types.get_mut(input_type_reference)
                else {
                    return Ok(());
                };
                input_object_type_referencers
                    .object_field_arguments
                    .shift_remove(self);
            }
            _ => {
                return Err(
                    SingleFederationError::Internal {
                        message: format!(
                            "Object field argument \"{}\"'s inner type \"{}\" does not refer to an existing input type.",
                            self,
                            input_type_reference.deref(),
                        )
                    }.into()
                );
            }
        }
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct InterfaceTypeDefinitionPosition {
    pub(crate) type_name: Name,
}

impl InterfaceTypeDefinitionPosition {
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

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<InterfaceType>, FederationError> {
        schema
            .types
            .get(&self.type_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Schema has no type \"{}\"", self),
                }
                .into()
            })
            .and_then(|type_| {
                if let ExtendedType::Interface(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(SingleFederationError::Internal {
                        message: format!("Schema type \"{}\" was not an interface", self),
                    }
                    .into())
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
    ) -> Result<&'schema mut Node<InterfaceType>, FederationError> {
        schema
            .types
            .get_mut(&self.type_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Schema has no type \"{}\"", self),
                }
                .into()
            })
            .and_then(|type_| {
                if let ExtendedType::Interface(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(SingleFederationError::Internal {
                        message: format!("Schema type \"{}\" was not an interface", self),
                    }
                    .into())
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
            // TODO: Allow built-in shadowing instead of ignoring them
            if is_graphql_reserved_name(&self.type_name) {
                return Ok(());
            }
            return Err(SingleFederationError::Internal {
                message: format!("Type \"{}\" has already been pre-inserted", self),
            }
            .into());
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
            return Err(SingleFederationError::Internal {
                message: format!("Type \"{}\" has not been pre-inserted", self),
            }
            .into());
        }
        if schema.schema.types.contains_key(&self.type_name) {
            // TODO: Allow built-in shadowing instead of ignoring them
            if is_graphql_reserved_name(&self.type_name) {
                return Ok(());
            }
            return Err(SingleFederationError::Internal {
                message: format!("Type \"{}\" already exists in schema", self),
            }
            .into());
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
                    message: format!("Schema missing referencers for type \"{}\"", self),
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

    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Component<Directive>,
    ) {
        let Some(type_) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        if !type_.directives.iter().any(|other_directive| {
            (other_directive.name == directive.name) && !other_directive.ptr_eq(directive)
        }) {
            self.remove_directive_name_references(&mut schema.referencers, &directive.name);
        }
        type_
            .make_mut()
            .directives
            .retain(|other_directive| !other_directive.ptr_eq(directive));
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
            schema,
            referencers,
            true,
        )?;
        for (field_name, field) in type_.fields.iter() {
            self.field(field_name.clone())
                .insert_references(field, schema, referencers, false)?;
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
            schema,
            referencers,
            true,
        )?;
        for (field_name, field) in type_.fields.iter() {
            self.field(field_name.clone())
                .remove_references(field, schema, referencers, false)?;
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
                    "Interface type \"{}\"'s directive application \"@{}\" does not refer to an existing directive.",
                    self,
                    name,
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
                    "Interface type \"{}\"'s implements \"{}\" does not refer to an existing interface.",
                    self,
                    name,
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
}

impl Display for InterfaceTypeDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.type_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct InterfaceFieldDefinitionPosition {
    pub(crate) type_name: Name,
    pub(crate) field_name: Name,
}

impl InterfaceFieldDefinitionPosition {
    pub(crate) fn is_introspection_typename_field(&self) -> bool {
        self.field_name == *INTROSPECTION_TYPENAME_FIELD_NAME
    }

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
    ) -> Result<&'schema Component<FieldDefinition>, FederationError> {
        let parent = self.parent();
        parent.get(schema)?;

        schema
            .type_field(&self.type_name, &self.field_name)
            .map_err(|_| {
                SingleFederationError::Internal {
                    message: format!(
                        "Interface type \"{}\" has no field \"{}\"",
                        parent, self.field_name
                    ),
                }
                .into()
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
    ) -> Result<&'schema mut Component<FieldDefinition>, FederationError> {
        let parent = self.parent();
        let type_ = parent.make_mut(schema)?.make_mut();

        if is_graphql_reserved_name(&self.field_name) {
            return Err(SingleFederationError::Internal {
                message: format!("Cannot mutate reserved interface field \"{}\"", self),
            }
            .into());
        }
        type_.fields.get_mut(&self.field_name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Interface type \"{}\" has no field \"{}\"",
                    parent, self.field_name
                ),
            }
            .into()
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
            return Err(SingleFederationError::Internal {
                message: format!("Interface field \"{}\" already exists in schema", self),
            }
            .into());
        }
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .fields
            .insert(self.field_name.clone(), field);
        self.insert_references(
            self.get(&schema.schema)?,
            &schema.schema,
            &mut schema.referencers,
            false,
        )
    }

    pub(crate) fn remove(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let Some(field) = self.try_get(&schema.schema) else {
            return Ok(());
        };
        self.remove_references(field, &schema.schema, &mut schema.referencers, false)?;
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .fields
            .shift_remove(&self.field_name);
        Ok(())
    }

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
        schema: &Schema,
        referencers: &mut Referencers,
        allow_built_ins: bool,
    ) -> Result<(), FederationError> {
        if !allow_built_ins && is_graphql_reserved_name(&self.field_name) {
            return Err(SingleFederationError::Internal {
                message: format!("Cannot insert reserved interface field \"{}\"", self),
            }
            .into());
        }
        validate_node_directives(field.directives.deref())?;
        for directive_reference in field.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        self.insert_type_references(field, schema, referencers)?;
        validate_arguments(&field.arguments)?;
        for argument in field.arguments.iter() {
            self.argument(argument.name.clone()).insert_references(
                argument,
                schema,
                referencers,
            )?;
        }
        Ok(())
    }

    fn remove_references(
        &self,
        field: &Component<FieldDefinition>,
        schema: &Schema,
        referencers: &mut Referencers,
        allow_built_ins: bool,
    ) -> Result<(), FederationError> {
        if !allow_built_ins && is_graphql_reserved_name(&self.field_name) {
            return Err(SingleFederationError::Internal {
                message: format!("Cannot remove reserved interface field \"{}\"", self),
            }
            .into());
        }
        for directive_reference in field.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
        self.remove_type_references(field, schema, referencers)?;
        for argument in field.arguments.iter() {
            self.argument(argument.name.clone()).remove_references(
                argument,
                schema,
                referencers,
            )?;
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
                    "Interface field \"{}\"'s directive application \"@{}\" does not refer to an existing directive.",
                    self,
                    name,
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
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let output_type_reference = field.ty.inner_named_type();
        match schema.types.get(output_type_reference) {
            Some(ExtendedType::Scalar(_)) => {
                let scalar_type_referencers = referencers
                    .scalar_types
                    .get_mut(output_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            output_type_reference
                        ),
                    })?;
                scalar_type_referencers
                    .interface_fields
                    .insert(self.clone());
            }
            Some(ExtendedType::Object(_)) => {
                let object_type_referencers = referencers
                    .object_types
                    .get_mut(output_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            output_type_reference
                        ),
                    })?;
                object_type_referencers
                    .interface_fields
                    .insert(self.clone());
            }
            Some(ExtendedType::Interface(_)) => {
                let interface_type_referencers = referencers
                    .interface_types
                    .get_mut(output_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            output_type_reference
                        ),
                    })?;
                interface_type_referencers
                    .interface_fields
                    .insert(self.clone());
            }
            Some(ExtendedType::Union(_)) => {
                let union_type_referencers = referencers
                    .union_types
                    .get_mut(output_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            output_type_reference
                        ),
                    })?;
                union_type_referencers.interface_fields.insert(self.clone());
            }
            Some(ExtendedType::Enum(_)) => {
                let enum_type_referencers = referencers
                    .enum_types
                    .get_mut(output_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                    message: format!(
                        "Schema missing referencers for type \"{}\"",
                        output_type_reference
                    ),
                })?;
                enum_type_referencers.interface_fields.insert(self.clone());
            }
            _ => {
                return Err(
                    SingleFederationError::Internal {
                        message: format!(
                            "Interface field \"{}\"'s inner type \"{}\" does not refer to an existing output type.",
                            self,
                            output_type_reference.deref(),
                        )
                    }.into()
                );
            }
        }
        Ok(())
    }

    fn remove_type_references(
        &self,
        field: &Component<FieldDefinition>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let output_type_reference = field.ty.inner_named_type();
        match schema.types.get(output_type_reference) {
            Some(ExtendedType::Scalar(_)) => {
                let Some(scalar_type_referencers) =
                    referencers.scalar_types.get_mut(output_type_reference)
                else {
                    return Ok(());
                };
                scalar_type_referencers.interface_fields.shift_remove(self);
            }
            Some(ExtendedType::Object(_)) => {
                let Some(object_type_referencers) =
                    referencers.object_types.get_mut(output_type_reference)
                else {
                    return Ok(());
                };
                object_type_referencers.interface_fields.shift_remove(self);
            }
            Some(ExtendedType::Interface(_)) => {
                let Some(interface_type_referencers) =
                    referencers.interface_types.get_mut(output_type_reference)
                else {
                    return Ok(());
                };
                interface_type_referencers
                    .interface_fields
                    .shift_remove(self);
            }
            Some(ExtendedType::Union(_)) => {
                let Some(union_type_referencers) =
                    referencers.union_types.get_mut(output_type_reference)
                else {
                    return Ok(());
                };
                union_type_referencers.interface_fields.shift_remove(self);
            }
            Some(ExtendedType::Enum(_)) => {
                let Some(enum_type_referencers) =
                    referencers.enum_types.get_mut(output_type_reference)
                else {
                    return Ok(());
                };
                enum_type_referencers.interface_fields.shift_remove(self);
            }
            _ => {
                return Err(
                    SingleFederationError::Internal {
                        message: format!(
                            "Interface field \"{}\"'s inner type \"{}\" does not refer to an existing output type.",
                            self,
                            output_type_reference.deref(),
                        )
                    }.into()
                );
            }
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
    pub(crate) fn parent(&self) -> InterfaceFieldDefinitionPosition {
        InterfaceFieldDefinitionPosition {
            type_name: self.type_name.clone(),
            field_name: self.field_name.clone(),
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<InputValueDefinition>, FederationError> {
        let parent = self.parent();
        let type_ = parent.get(schema)?;

        type_
            .arguments
            .iter()
            .find(|a| a.name == self.argument_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!(
                        "Interface field \"{}\" has no argument \"{}\"",
                        parent, self.argument_name
                    ),
                }
                .into()
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
    ) -> Result<&'schema mut Node<InputValueDefinition>, FederationError> {
        let parent = self.parent();
        let type_ = parent.make_mut(schema)?.make_mut();

        type_
            .arguments
            .iter_mut()
            .find(|a| a.name == self.argument_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!(
                        "Interface field \"{}\" has no argument \"{}\"",
                        parent, self.argument_name
                    ),
                }
                .into()
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

    pub(crate) fn insert(
        &self,
        schema: &mut FederationSchema,
        argument: Node<InputValueDefinition>,
    ) -> Result<(), FederationError> {
        if self.argument_name != argument.name {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Interface field argument \"{}\" given argument named \"{}\"",
                    self, argument.name,
                ),
            }
            .into());
        }
        if self.try_get(&schema.schema).is_some() {
            // TODO: Handle old spec edge case of arguments with non-unique names
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Interface field argument \"{}\" already exists in schema",
                    self,
                ),
            }
            .into());
        }
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .arguments
            .push(argument);
        self.insert_references(
            self.get(&schema.schema)?,
            &schema.schema,
            &mut schema.referencers,
        )
    }
    pub(crate) fn remove(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let Some(argument) = self.try_get(&schema.schema) else {
            return Ok(());
        };
        self.remove_references(argument, &schema.schema, &mut schema.referencers)?;
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

    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Node<Directive>,
    ) {
        let Some(argument) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        if !argument.directives.iter().any(|other_directive| {
            (other_directive.name == directive.name) && !other_directive.ptr_eq(directive)
        }) {
            self.remove_directive_name_references(&mut schema.referencers, &directive.name);
        }
        argument
            .make_mut()
            .directives
            .retain(|other_directive| !other_directive.ptr_eq(directive));
    }

    fn insert_references(
        &self,
        argument: &Node<InputValueDefinition>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if is_graphql_reserved_name(&self.argument_name) {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Cannot insert reserved interface field argument \"{}\"",
                    self
                ),
            }
            .into());
        }
        validate_node_directives(argument.directives.deref())?;
        for directive_reference in argument.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        self.insert_type_references(argument, schema, referencers)
    }

    fn remove_references(
        &self,
        argument: &Node<InputValueDefinition>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if is_graphql_reserved_name(&self.argument_name) {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Cannot remove reserved interface field argument \"{}\"",
                    self
                ),
            }
            .into());
        }
        for directive_reference in argument.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
        self.remove_type_references(argument, schema, referencers)
    }

    fn insert_directive_name_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let directive_referencers = referencers.directives.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Interface field argument \"{}\"'s directive application \"@{}\" does not refer to an existing directive.",
                    self,
                    name,
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
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let input_type_reference = argument.ty.inner_named_type();
        match schema.types.get(input_type_reference) {
            Some(ExtendedType::Scalar(_)) => {
                let scalar_type_referencers = referencers
                    .scalar_types
                    .get_mut(input_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            input_type_reference
                        ),
                    })?;
                scalar_type_referencers
                    .interface_field_arguments
                    .insert(self.clone());
            }
            Some(ExtendedType::Enum(_)) => {
                let enum_type_referencers = referencers
                    .enum_types
                    .get_mut(input_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            input_type_reference
                        ),
                    })?;
                enum_type_referencers
                    .interface_field_arguments
                    .insert(self.clone());
            }
            Some(ExtendedType::InputObject(_)) => {
                let input_object_type_referencers = referencers
                    .input_object_types
                    .get_mut(input_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            input_type_reference
                        ),
                    })?;
                input_object_type_referencers
                    .interface_field_arguments
                    .insert(self.clone());
            }
            _ => {
                return Err(
                    SingleFederationError::Internal {
                        message: format!(
                            "Interface field argument \"{}\"'s inner type \"{}\" does not refer to an existing input type.",
                            self,
                            input_type_reference.deref(),
                        )
                    }.into()
                );
            }
        }
        Ok(())
    }

    fn remove_type_references(
        &self,
        argument: &Node<InputValueDefinition>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let input_type_reference = argument.ty.inner_named_type();
        match schema.types.get(input_type_reference) {
            Some(ExtendedType::Scalar(_)) => {
                let Some(scalar_type_referencers) =
                    referencers.scalar_types.get_mut(input_type_reference)
                else {
                    return Ok(());
                };
                scalar_type_referencers
                    .interface_field_arguments
                    .shift_remove(self);
            }
            Some(ExtendedType::Enum(_)) => {
                let Some(enum_type_referencers) =
                    referencers.enum_types.get_mut(input_type_reference)
                else {
                    return Ok(());
                };
                enum_type_referencers
                    .interface_field_arguments
                    .shift_remove(self);
            }
            Some(ExtendedType::InputObject(_)) => {
                let Some(input_object_type_referencers) =
                    referencers.input_object_types.get_mut(input_type_reference)
                else {
                    return Ok(());
                };
                input_object_type_referencers
                    .interface_field_arguments
                    .shift_remove(self);
            }
            _ => {
                return Err(
                    SingleFederationError::Internal {
                        message: format!(
                            "Interface field argument \"{}\"'s inner type \"{}\" does not refer to an existing input type.",
                            self,
                            input_type_reference.deref(),
                        )
                    }.into()
                );
            }
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct UnionTypeDefinitionPosition {
    pub(crate) type_name: Name,
}

impl UnionTypeDefinitionPosition {
    pub(crate) fn new(type_name: Name) -> Self {
        Self { type_name }
    }

    pub(crate) fn introspection_typename_field(&self) -> UnionTypenameFieldDefinitionPosition {
        UnionTypenameFieldDefinitionPosition {
            type_name: self.type_name.clone(),
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<UnionType>, FederationError> {
        schema
            .types
            .get(&self.type_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Schema has no type \"{}\"", self),
                }
                .into()
            })
            .and_then(|type_| {
                if let ExtendedType::Union(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(SingleFederationError::Internal {
                        message: format!("Schema type \"{}\" was not an union", self),
                    }
                    .into())
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
    ) -> Result<&'schema mut Node<UnionType>, FederationError> {
        schema
            .types
            .get_mut(&self.type_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Schema has no type \"{}\"", self),
                }
                .into()
            })
            .and_then(|type_| {
                if let ExtendedType::Union(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(SingleFederationError::Internal {
                        message: format!("Schema type \"{}\" was not an union", self),
                    }
                    .into())
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
            // TODO: Allow built-in shadowing instead of ignoring them
            if is_graphql_reserved_name(&self.type_name) {
                return Ok(());
            }
            return Err(SingleFederationError::Internal {
                message: format!("Type \"{}\" has already been pre-inserted", self),
            }
            .into());
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
            return Err(SingleFederationError::Internal {
                message: format!("Type \"{}\" has not been pre-inserted", self),
            }
            .into());
        }
        if schema.schema.types.contains_key(&self.type_name) {
            // TODO: Allow built-in shadowing instead of ignoring them
            if is_graphql_reserved_name(&self.type_name) {
                return Ok(());
            }
            return Err(SingleFederationError::Internal {
                message: format!("Type \"{}\" already exists in schema", self),
            }
            .into());
        }
        schema
            .schema
            .types
            .insert(self.type_name.clone(), ExtendedType::Union(type_));
        self.insert_references(self.get(&schema.schema)?, &mut schema.referencers)
    }

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
        self.remove_references(type_, &mut schema.referencers)?;
        schema.schema.types.shift_remove(&self.type_name);
        Ok(Some(
            schema
                .referencers
                .union_types
                .shift_remove(&self.type_name)
                .ok_or_else(|| SingleFederationError::Internal {
                    message: format!("Schema missing referencers for type \"{}\"", self),
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

    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Component<Directive>,
    ) {
        let Some(type_) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        if !type_.directives.iter().any(|other_directive| {
            (other_directive.name == directive.name) && !other_directive.ptr_eq(directive)
        }) {
            self.remove_directive_name_references(&mut schema.referencers, &directive.name);
        }
        type_
            .make_mut()
            .directives
            .retain(|other_directive| !other_directive.ptr_eq(directive));
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

    fn remove_references(
        &self,
        type_: &Node<UnionType>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
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
                    "Union type \"{}\"'s directive application \"@{}\" does not refer to an existing directive.",
                    self,
                    name,
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
                    "Union type \"{}\"'s member \"{}\" does not refer to an existing object.",
                    self, name,
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
}

impl Display for UnionTypeDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.type_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    ) -> Result<&'schema Component<FieldDefinition>, FederationError> {
        let parent = self.parent();
        parent.get(schema)?;

        schema
            .type_field(&self.type_name, self.field_name())
            .map_err(|_| {
                SingleFederationError::Internal {
                    message: format!(
                        "Union type \"{}\" has no field \"{}\"",
                        parent,
                        self.field_name()
                    ),
                }
                .into()
            })
    }

    pub(crate) fn try_get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Option<&'schema Component<FieldDefinition>> {
        self.get(schema).ok()
    }

    fn insert_references(&self, referencers: &mut Referencers) -> Result<(), FederationError> {
        self.insert_type_references(referencers)?;
        Ok(())
    }

    fn remove_references(&self, referencers: &mut Referencers) -> Result<(), FederationError> {
        self.remove_type_references(referencers)?;
        Ok(())
    }

    fn insert_type_references(&self, referencers: &mut Referencers) -> Result<(), FederationError> {
        let output_type_reference = "String";
        let scalar_type_referencers = referencers
            .scalar_types
            .get_mut(output_type_reference)
            .ok_or_else(|| SingleFederationError::Internal {
                message: format!(
                    "Schema missing referencers for type \"{}\"",
                    output_type_reference
                ),
            })?;
        scalar_type_referencers.union_fields.insert(self.clone());
        Ok(())
    }

    fn remove_type_references(&self, referencers: &mut Referencers) -> Result<(), FederationError> {
        let output_type_reference = "String";
        let Some(scalar_type_referencers) = referencers.scalar_types.get_mut(output_type_reference)
        else {
            return Ok(());
        };
        scalar_type_referencers.union_fields.shift_remove(self);
        Ok(())
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
    pub(crate) fn value(&self, value_name: Name) -> EnumValueDefinitionPosition {
        EnumValueDefinitionPosition {
            type_name: self.type_name.clone(),
            value_name,
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<EnumType>, FederationError> {
        schema
            .types
            .get(&self.type_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Schema has no type \"{}\"", self),
                }
                .into()
            })
            .and_then(|type_| {
                if let ExtendedType::Enum(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(SingleFederationError::Internal {
                        message: format!("Schema type \"{}\" was not an enum", self),
                    }
                    .into())
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
    ) -> Result<&'schema mut Node<EnumType>, FederationError> {
        schema
            .types
            .get_mut(&self.type_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Schema has no type \"{}\"", self),
                }
                .into()
            })
            .and_then(|type_| {
                if let ExtendedType::Enum(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(SingleFederationError::Internal {
                        message: format!("Schema type \"{}\" was not an enum", self),
                    }
                    .into())
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
            // TODO: Allow built-in shadowing instead of ignoring them
            if is_graphql_reserved_name(&self.type_name) {
                return Ok(());
            }
            return Err(SingleFederationError::Internal {
                message: format!("Type \"{}\" has already been pre-inserted", self),
            }
            .into());
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
            return Err(SingleFederationError::Internal {
                message: format!("Type \"{}\" has not been pre-inserted", self),
            }
            .into());
        }
        if schema.schema.types.contains_key(&self.type_name) {
            // TODO: Allow built-in shadowing instead of ignoring them
            if is_graphql_reserved_name(&self.type_name) {
                return Ok(());
            }
            return Err(SingleFederationError::Internal {
                message: format!("Type \"{}\" already exists in schema", self),
            }
            .into());
        }
        schema
            .schema
            .types
            .insert(self.type_name.clone(), ExtendedType::Enum(type_));
        self.insert_references(self.get(&schema.schema)?, &mut schema.referencers)
    }

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
        for argument in referencers.object_field_arguments {
            argument.remove(schema)?;
        }
        for field in referencers.interface_fields {
            field.remove_recursive(schema)?;
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
                    message: format!("Schema missing referencers for type \"{}\"", self),
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

    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Component<Directive>,
    ) {
        let Some(type_) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        if !type_.directives.iter().any(|other_directive| {
            (other_directive.name == directive.name) && !other_directive.ptr_eq(directive)
        }) {
            self.remove_directive_name_references(&mut schema.referencers, &directive.name);
        }
        type_
            .make_mut()
            .directives
            .retain(|other_directive| !other_directive.ptr_eq(directive));
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
                    "Enum type \"{}\"'s directive application \"@{}\" does not refer to an existing directive.",
                    self,
                    name,
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
    pub(crate) fn parent(&self) -> EnumTypeDefinitionPosition {
        EnumTypeDefinitionPosition {
            type_name: self.type_name.clone(),
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Component<EnumValueDefinition>, FederationError> {
        let parent = self.parent();
        let type_ = parent.get(schema)?;

        type_.values.get(&self.value_name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Enum type \"{}\" has no value \"{}\"",
                    parent, self.value_name
                ),
            }
            .into()
        })
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
    ) -> Result<&'schema mut Component<EnumValueDefinition>, FederationError> {
        let parent = self.parent();
        let type_ = parent.make_mut(schema)?.make_mut();

        type_.values.get_mut(&self.value_name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Enum type \"{}\" has no value \"{}\"",
                    parent, self.value_name
                ),
            }
            .into()
        })
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
            return Err(SingleFederationError::Internal {
                message: format!("Enum value \"{}\" already exists in schema", self,),
            }
            .into());
        }
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .values
            .insert(self.value_name.clone(), value);
        self.insert_references(self.get(&schema.schema)?, &mut schema.referencers)
    }

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

    pub(crate) fn remove_recursive(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        self.remove(schema)?;
        let parent = self.parent();
        let Some(type_) = parent.try_get(&schema.schema) else {
            return Ok(());
        };
        if type_.values.is_empty() {
            parent.remove_recursive(schema)?;
        }
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

    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Node<Directive>,
    ) {
        let Some(value) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        if !value.directives.iter().any(|other_directive| {
            (other_directive.name == directive.name) && !other_directive.ptr_eq(directive)
        }) {
            self.remove_directive_name_references(&mut schema.referencers, &directive.name);
        }
        value
            .make_mut()
            .directives
            .retain(|other_directive| !other_directive.ptr_eq(directive));
    }

    fn insert_references(
        &self,
        value: &Component<EnumValueDefinition>,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if is_graphql_reserved_name(&self.value_name) {
            return Err(SingleFederationError::Internal {
                message: format!("Cannot insert reserved enum value \"{}\"", self),
            }
            .into());
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
            return Err(SingleFederationError::Internal {
                message: format!("Cannot remove reserved enum value \"{}\"", self),
            }
            .into());
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
                    "Enum value \"{}\"'s directive application \"@{}\" does not refer to an existing directive.",
                    self,
                    name,
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
    pub(crate) fn field(&self, field_name: Name) -> InputObjectFieldDefinitionPosition {
        InputObjectFieldDefinitionPosition {
            type_name: self.type_name.clone(),
            field_name,
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<InputObjectType>, FederationError> {
        schema
            .types
            .get(&self.type_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Schema has no type \"{}\"", self),
                }
                .into()
            })
            .and_then(|type_| {
                if let ExtendedType::InputObject(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(SingleFederationError::Internal {
                        message: format!("Schema type \"{}\" was not an input object", self),
                    }
                    .into())
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
    ) -> Result<&'schema mut Node<InputObjectType>, FederationError> {
        schema
            .types
            .get_mut(&self.type_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Schema has no type \"{}\"", self),
                }
                .into()
            })
            .and_then(|type_| {
                if let ExtendedType::InputObject(type_) = type_ {
                    Ok(type_)
                } else {
                    Err(SingleFederationError::Internal {
                        message: format!("Schema type \"{}\" was not an input object", self),
                    }
                    .into())
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
            // TODO: Allow built-in shadowing instead of ignoring them
            if is_graphql_reserved_name(&self.type_name) {
                return Ok(());
            }
            return Err(SingleFederationError::Internal {
                message: format!("Type \"{}\" has already been pre-inserted", self),
            }
            .into());
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
            return Err(SingleFederationError::Internal {
                message: format!("Type \"{}\" has not been pre-inserted", self),
            }
            .into());
        }
        if schema.schema.types.contains_key(&self.type_name) {
            // TODO: Allow built-in shadowing instead of ignoring them
            if is_graphql_reserved_name(&self.type_name) {
                return Ok(());
            }
            return Err(SingleFederationError::Internal {
                message: format!("Type \"{}\" already exists in schema", self),
            }
            .into());
        }
        schema
            .schema
            .types
            .insert(self.type_name.clone(), ExtendedType::InputObject(type_));
        self.insert_references(
            self.get(&schema.schema)?,
            &schema.schema,
            &mut schema.referencers,
        )
    }

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
        self.remove_references(type_, &schema.schema, &mut schema.referencers)?;
        schema.schema.types.shift_remove(&self.type_name);
        Ok(Some(
            schema
                .referencers
                .input_object_types
                .shift_remove(&self.type_name)
                .ok_or_else(|| SingleFederationError::Internal {
                    message: format!("Schema missing referencers for type \"{}\"", self),
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

    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Component<Directive>,
    ) {
        let Some(type_) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        if !type_.directives.iter().any(|other_directive| {
            (other_directive.name == directive.name) && !other_directive.ptr_eq(directive)
        }) {
            self.remove_directive_name_references(&mut schema.referencers, &directive.name);
        }
        type_
            .make_mut()
            .directives
            .retain(|other_directive| !other_directive.ptr_eq(directive));
    }

    fn insert_references(
        &self,
        type_: &Node<InputObjectType>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        validate_component_directives(type_.directives.deref())?;
        for directive_reference in type_.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        for (field_name, field) in type_.fields.iter() {
            self.field(field_name.clone())
                .insert_references(field, schema, referencers)?;
        }
        Ok(())
    }

    fn remove_references(
        &self,
        type_: &Node<InputObjectType>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        for directive_reference in type_.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
        for (field_name, field) in type_.fields.iter() {
            self.field(field_name.clone())
                .remove_references(field, schema, referencers)?;
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
                    "Input object type \"{}\"'s directive application \"@{}\" does not refer to an existing directive.",
                    self,
                    name,
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
    pub(crate) fn parent(&self) -> InputObjectTypeDefinitionPosition {
        InputObjectTypeDefinitionPosition {
            type_name: self.type_name.clone(),
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Component<InputValueDefinition>, FederationError> {
        let parent = self.parent();
        let type_ = parent.get(schema)?;

        type_.fields.get(&self.field_name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Input object type \"{}\" has no field \"{}\"",
                    parent, self.field_name
                ),
            }
            .into()
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
    ) -> Result<&'schema mut Component<InputValueDefinition>, FederationError> {
        let parent = self.parent();
        let type_ = parent.make_mut(schema)?.make_mut();

        type_.fields.get_mut(&self.field_name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Input object type \"{}\" has no field \"{}\"",
                    parent, self.field_name
                ),
            }
            .into()
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
            return Err(SingleFederationError::Internal {
                message: format!("Input object field \"{}\" already exists in schema", self),
            }
            .into());
        }
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .fields
            .insert(self.field_name.clone(), field);
        self.insert_references(
            self.get(&schema.schema)?,
            &schema.schema,
            &mut schema.referencers,
        )
    }

    pub(crate) fn remove(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let Some(field) = self.try_get(&schema.schema) else {
            return Ok(());
        };
        self.remove_references(field, &schema.schema, &mut schema.referencers)?;
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .fields
            .shift_remove(&self.field_name);
        Ok(())
    }

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
        field: &Component<InputValueDefinition>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if is_graphql_reserved_name(&self.field_name) {
            return Err(SingleFederationError::Internal {
                message: format!("Cannot insert reserved input object field \"{}\"", self),
            }
            .into());
        }
        validate_node_directives(field.directives.deref())?;
        for directive_reference in field.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        self.insert_type_references(field, schema, referencers)
    }

    fn remove_references(
        &self,
        field: &Component<InputValueDefinition>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if is_graphql_reserved_name(&self.field_name) {
            return Err(SingleFederationError::Internal {
                message: format!("Cannot remove reserved input object field \"{}\"", self),
            }
            .into());
        }
        for directive_reference in field.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
        self.remove_type_references(field, schema, referencers)
    }

    fn insert_directive_name_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let directive_referencers = referencers.directives.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Input object field \"{}\"'s directive application \"@{}\" does not refer to an existing directive.",
                    self,
                    name,
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
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let input_type_reference = argument.ty.inner_named_type();
        match schema.types.get(input_type_reference) {
            Some(ExtendedType::Scalar(_)) => {
                let scalar_type_referencers = referencers
                    .scalar_types
                    .get_mut(input_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            input_type_reference
                        ),
                    })?;
                scalar_type_referencers
                    .input_object_fields
                    .insert(self.clone());
            }
            Some(ExtendedType::Enum(_)) => {
                let enum_type_referencers = referencers
                    .enum_types
                    .get_mut(input_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            input_type_reference
                        ),
                    })?;
                enum_type_referencers
                    .input_object_fields
                    .insert(self.clone());
            }
            Some(ExtendedType::InputObject(_)) => {
                let input_object_type_referencers = referencers
                    .input_object_types
                    .get_mut(input_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            input_type_reference
                        ),
                    })?;
                input_object_type_referencers
                    .input_object_fields
                    .insert(self.clone());
            }
            _ => {
                return Err(
                    SingleFederationError::Internal {
                        message: format!(
                            "Input object field \"{}\"'s inner type \"{}\" does not refer to an existing input type.",
                            self,
                            input_type_reference.deref(),
                        )
                    }.into()
                );
            }
        }
        Ok(())
    }

    fn remove_type_references(
        &self,
        field: &Component<InputValueDefinition>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let input_type_reference = field.ty.inner_named_type();
        match schema.types.get(input_type_reference) {
            Some(ExtendedType::Scalar(_)) => {
                let Some(scalar_type_referencers) =
                    referencers.scalar_types.get_mut(input_type_reference)
                else {
                    return Ok(());
                };
                scalar_type_referencers
                    .input_object_fields
                    .shift_remove(self);
            }
            Some(ExtendedType::Enum(_)) => {
                let Some(enum_type_referencers) =
                    referencers.enum_types.get_mut(input_type_reference)
                else {
                    return Ok(());
                };
                enum_type_referencers.input_object_fields.shift_remove(self);
            }
            Some(ExtendedType::InputObject(_)) => {
                let Some(input_object_type_referencers) =
                    referencers.input_object_types.get_mut(input_type_reference)
                else {
                    return Ok(());
                };
                input_object_type_referencers
                    .input_object_fields
                    .shift_remove(self);
            }
            _ => {
                return Err(
                    SingleFederationError::Internal {
                        message: format!(
                            "Input object field \"{}\"'s inner type \"{}\" does not refer to an existing input type.",
                            self,
                            input_type_reference.deref(),
                        )
                    }.into()
                );
            }
        }
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
    ) -> Result<&'schema Node<DirectiveDefinition>, FederationError> {
        schema
            .directive_definitions
            .get(&self.directive_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Schema has no directive \"{}\"", self),
                }
                .into()
            })
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
    ) -> Result<&'schema mut Node<DirectiveDefinition>, FederationError> {
        schema
            .directive_definitions
            .get_mut(&self.directive_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!("Schema has no directive \"{}\"", self),
                }
                .into()
            })
    }

    fn try_make_mut<'schema>(
        &self,
        schema: &'schema mut Schema,
    ) -> Option<&'schema mut Node<DirectiveDefinition>> {
        if self.try_get(schema).is_some() {
            self.make_mut(schema).ok()
        } else {
            None
        }
    }

    pub(crate) fn pre_insert(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        if schema
            .referencers
            .directives
            .contains_key(&self.directive_name)
        {
            // TODO: Allow built-in shadowing instead of ignoring them
            if is_graphql_reserved_name(&self.directive_name)
                || GRAPHQL_BUILTIN_DIRECTIVE_NAMES.contains(&self.directive_name)
            {
                return Ok(());
            }
            return Err(SingleFederationError::Internal {
                message: format!("Directive \"{}\" has already been pre-inserted", self),
            }
            .into());
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
            return Err(SingleFederationError::Internal {
                message: format!("Directive \"{}\" has not been pre-inserted", self),
            }
            .into());
        }
        if schema
            .schema
            .directive_definitions
            .contains_key(&self.directive_name)
        {
            // TODO: Allow built-in shadowing instead of ignoring them
            if is_graphql_reserved_name(&self.directive_name)
                || GRAPHQL_BUILTIN_DIRECTIVE_NAMES.contains(&self.directive_name)
            {
                return Ok(());
            }
            return Err(SingleFederationError::Internal {
                message: format!("Directive \"{}\" already exists in schema", self),
            }
            .into());
        }
        schema
            .schema
            .directive_definitions
            .insert(self.directive_name.clone(), directive);
        self.insert_references(
            self.get(&schema.schema)?,
            &schema.schema,
            &mut schema.referencers,
        )
    }

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
        self.remove_references(directive, &schema.schema, &mut schema.referencers)?;
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
                    message: format!("Schema missing referencers for directive \"{}\"", self),
                })?,
        ))
    }

    fn insert_references(
        &self,
        directive: &Node<DirectiveDefinition>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        for argument in directive.arguments.iter() {
            self.argument(argument.name.clone()).insert_references(
                argument,
                schema,
                referencers,
            )?;
        }
        Ok(())
    }

    fn remove_references(
        &self,
        directive: &Node<DirectiveDefinition>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        for argument in directive.arguments.iter() {
            self.argument(argument.name.clone()).remove_references(
                argument,
                schema,
                referencers,
            )?;
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
    pub(crate) fn parent(&self) -> DirectiveDefinitionPosition {
        DirectiveDefinitionPosition {
            directive_name: self.directive_name.clone(),
        }
    }

    pub(crate) fn get<'schema>(
        &self,
        schema: &'schema Schema,
    ) -> Result<&'schema Node<InputValueDefinition>, FederationError> {
        let parent = self.parent();
        let type_ = parent.get(schema)?;

        type_
            .arguments
            .iter()
            .find(|a| a.name == self.argument_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!(
                        "Directive \"{}\" has no argument \"{}\"",
                        parent, self.argument_name
                    ),
                }
                .into()
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
    ) -> Result<&'schema mut Node<InputValueDefinition>, FederationError> {
        let parent = self.parent();
        let type_ = parent.make_mut(schema)?.make_mut();

        type_
            .arguments
            .iter_mut()
            .find(|a| a.name == self.argument_name)
            .ok_or_else(|| {
                SingleFederationError::Internal {
                    message: format!(
                        "Directive \"{}\" has no argument \"{}\"",
                        parent, self.argument_name
                    ),
                }
                .into()
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

    pub(crate) fn insert(
        &self,
        schema: &mut FederationSchema,
        argument: Node<InputValueDefinition>,
    ) -> Result<(), FederationError> {
        if self.argument_name != argument.name {
            return Err(SingleFederationError::Internal {
                message: format!(
                    "Directive argument \"{}\" given argument named \"{}\"",
                    self, argument.name,
                ),
            }
            .into());
        }
        if self.try_get(&schema.schema).is_some() {
            // TODO: Handle old spec edge case of arguments with non-unique names
            return Err(SingleFederationError::Internal {
                message: format!("Directive argument \"{}\" already exists in schema", self,),
            }
            .into());
        }
        self.parent()
            .make_mut(&mut schema.schema)?
            .make_mut()
            .arguments
            .push(argument);
        self.insert_references(
            self.get(&schema.schema)?,
            &schema.schema,
            &mut schema.referencers,
        )
    }

    pub(crate) fn remove(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let Some(argument) = self.try_get(&schema.schema) else {
            return Ok(());
        };
        self.remove_references(argument, &schema.schema, &mut schema.referencers)?;
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

    pub(crate) fn remove_directive(
        &self,
        schema: &mut FederationSchema,
        directive: &Node<Directive>,
    ) {
        let Some(argument) = self.try_make_mut(&mut schema.schema) else {
            return;
        };
        if !argument.directives.iter().any(|other_directive| {
            (other_directive.name == directive.name) && !other_directive.ptr_eq(directive)
        }) {
            self.remove_directive_name_references(&mut schema.referencers, &directive.name);
        }
        argument
            .make_mut()
            .directives
            .retain(|other_directive| !other_directive.ptr_eq(directive));
    }

    fn insert_references(
        &self,
        argument: &Node<InputValueDefinition>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if is_graphql_reserved_name(&self.argument_name) {
            return Err(SingleFederationError::Internal {
                message: format!("Cannot insert reserved directive argument \"{}\"", self),
            }
            .into());
        }
        validate_node_directives(argument.directives.deref())?;
        for directive_reference in argument.directives.iter() {
            self.insert_directive_name_references(referencers, &directive_reference.name)?;
        }
        self.insert_type_references(argument, schema, referencers)
    }

    fn remove_references(
        &self,
        argument: &Node<InputValueDefinition>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        if is_graphql_reserved_name(&self.argument_name) {
            return Err(SingleFederationError::Internal {
                message: format!("Cannot remove reserved directive argument \"{}\"", self),
            }
            .into());
        }
        for directive_reference in argument.directives.iter() {
            self.remove_directive_name_references(referencers, &directive_reference.name);
        }
        self.remove_type_references(argument, schema, referencers)
    }

    fn insert_directive_name_references(
        &self,
        referencers: &mut Referencers,
        name: &Name,
    ) -> Result<(), FederationError> {
        let directive_referencers = referencers.directives.get_mut(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: format!(
                    "Directive argument \"{}\"'s directive application \"@{}\" does not refer to an existing directive.",
                    self,
                    name,
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
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let input_type_reference = argument.ty.inner_named_type();
        match schema.types.get(input_type_reference) {
            Some(ExtendedType::Scalar(_)) => {
                let scalar_type_referencers = referencers
                    .scalar_types
                    .get_mut(input_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            input_type_reference
                        ),
                    })?;
                scalar_type_referencers
                    .directive_arguments
                    .insert(self.clone());
            }
            Some(ExtendedType::Enum(_)) => {
                let enum_type_referencers = referencers
                    .enum_types
                    .get_mut(input_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            input_type_reference
                        ),
                    })?;
                enum_type_referencers
                    .directive_arguments
                    .insert(self.clone());
            }
            Some(ExtendedType::InputObject(_)) => {
                let input_object_type_referencers = referencers
                    .input_object_types
                    .get_mut(input_type_reference)
                    .ok_or_else(|| SingleFederationError::Internal {
                        message: format!(
                            "Schema missing referencers for type \"{}\"",
                            input_type_reference
                        ),
                    })?;
                input_object_type_referencers
                    .directive_arguments
                    .insert(self.clone());
            }
            _ => {
                return Err(
                    SingleFederationError::Internal {
                        message: format!(
                            "Directive argument \"{}\"'s inner type \"{}\" does not refer to an existing input type.",
                            self,
                            input_type_reference.deref(),
                        )
                    }.into()
                );
            }
        }
        Ok(())
    }

    fn remove_type_references(
        &self,
        argument: &Node<InputValueDefinition>,
        schema: &Schema,
        referencers: &mut Referencers,
    ) -> Result<(), FederationError> {
        let input_type_reference = argument.ty.inner_named_type();
        match schema.types.get(input_type_reference) {
            Some(ExtendedType::Scalar(_)) => {
                let Some(scalar_type_referencers) =
                    referencers.scalar_types.get_mut(input_type_reference)
                else {
                    return Ok(());
                };
                scalar_type_referencers
                    .directive_arguments
                    .shift_remove(self);
            }
            Some(ExtendedType::Enum(_)) => {
                let Some(enum_type_referencers) =
                    referencers.enum_types.get_mut(input_type_reference)
                else {
                    return Ok(());
                };
                enum_type_referencers.directive_arguments.shift_remove(self);
            }
            Some(ExtendedType::InputObject(_)) => {
                let Some(input_object_type_referencers) =
                    referencers.input_object_types.get_mut(input_type_reference)
                else {
                    return Ok(());
                };
                input_object_type_referencers
                    .directive_arguments
                    .shift_remove(self);
            }
            _ => {
                return Err(
                    SingleFederationError::Internal {
                        message: format!(
                            "Directive argument \"{}\"'s inner type \"{}\" does not refer to an existing input type.",
                            self,
                            input_type_reference.deref(),
                        )
                    }.into()
                );
            }
        }
        Ok(())
    }
}

impl Display for DirectiveArgumentDefinitionPosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "@{}({}:)", self.directive_name, self.argument_name)
    }
}

pub(crate) fn is_graphql_reserved_name(name: &str) -> bool {
    name.starts_with("__")
}

lazy_static! {
    static ref GRAPHQL_BUILTIN_SCALAR_NAMES: IndexSet<Name> = {
        IndexSet::from([
            name!("Int"),
            name!("Float"),
            name!("String"),
            name!("Boolean"),
            name!("ID"),
        ])
    };
    static ref GRAPHQL_BUILTIN_DIRECTIVE_NAMES: IndexSet<Name> = {
        IndexSet::from([
            name!("include"),
            name!("skip"),
            name!("deprecated"),
            name!("specifiedBy"),
            name!("defer"),
        ])
    };
    // This is static so that UnionTypenameFieldDefinitionPosition.field_name() can return `&Name`,
    // like the other field_name() methods in this file.
    pub(crate) static ref INTROSPECTION_TYPENAME_FIELD_NAME: Name = name!("__typename");
}

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

impl FederationSchema {
    /// Note that the input schema must be partially valid, in that:
    /// 1. All schema element references must point to an existing schema element of the appropriate
    ///    kind (e.g. object type fields must return an existing output type).
    /// 2. If the schema uses the core/link spec, then usages of the @core/@link directive must be
    ///    valid.
    /// The input schema may be otherwise invalid GraphQL (e.g. it may not contain a Query type). If
    /// you want a ValidFederationSchema, use ValidFederationSchema::new() instead.
    pub(crate) fn new(schema: Schema) -> Result<FederationSchema, FederationError> {
        let metadata = links_metadata(&schema)?;
        let mut referencers: Referencers = Default::default();

        // Shallow pass to populate referencers for types/directives.
        for (type_name, type_) in schema.types.iter() {
            match type_ {
                ExtendedType::Scalar(_) => {
                    referencers
                        .scalar_types
                        .insert(type_name.clone(), Default::default());
                }
                ExtendedType::Object(_) => {
                    referencers
                        .object_types
                        .insert(type_name.clone(), Default::default());
                }
                ExtendedType::Interface(_) => {
                    referencers
                        .interface_types
                        .insert(type_name.clone(), Default::default());
                }
                ExtendedType::Union(_) => {
                    referencers
                        .union_types
                        .insert(type_name.clone(), Default::default());
                }
                ExtendedType::Enum(_) => {
                    referencers
                        .enum_types
                        .insert(type_name.clone(), Default::default());
                }
                ExtendedType::InputObject(_) => {
                    referencers
                        .input_object_types
                        .insert(type_name.clone(), Default::default());
                }
            }
        }
        for directive_name in schema.directive_definitions.keys() {
            referencers
                .directives
                .insert(directive_name.clone(), Default::default());
        }

        // Deep pass to find references.
        SchemaDefinitionPosition.insert_references(
            &schema.schema_definition,
            &schema,
            &mut referencers,
        )?;
        for (type_name, type_) in schema.types.iter() {
            match type_ {
                ExtendedType::Scalar(type_) => {
                    ScalarTypeDefinitionPosition {
                        type_name: type_name.clone(),
                    }
                    .insert_references(type_, &mut referencers)?;
                }
                ExtendedType::Object(type_) => {
                    ObjectTypeDefinitionPosition {
                        type_name: type_name.clone(),
                    }
                    .insert_references(type_, &schema, &mut referencers)?;
                }
                ExtendedType::Interface(type_) => {
                    InterfaceTypeDefinitionPosition {
                        type_name: type_name.clone(),
                    }
                    .insert_references(type_, &schema, &mut referencers)?;
                }
                ExtendedType::Union(type_) => {
                    UnionTypeDefinitionPosition {
                        type_name: type_name.clone(),
                    }
                    .insert_references(type_, &mut referencers)?;
                }
                ExtendedType::Enum(type_) => {
                    EnumTypeDefinitionPosition {
                        type_name: type_name.clone(),
                    }
                    .insert_references(type_, &mut referencers)?;
                }
                ExtendedType::InputObject(type_) => {
                    InputObjectTypeDefinitionPosition {
                        type_name: type_name.clone(),
                    }
                    .insert_references(type_, &schema, &mut referencers)?;
                }
            }
        }
        for (directive_name, directive) in schema.directive_definitions.iter() {
            DirectiveDefinitionPosition {
                directive_name: directive_name.clone(),
            }
            .insert_references(directive, &schema, &mut referencers)?;
        }

        Ok(FederationSchema {
            schema,
            referencers,
            links_metadata: metadata.map(Box::new),
            subgraph_metadata: None,
        })
    }
}
