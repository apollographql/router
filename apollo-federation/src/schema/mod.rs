use crate::error::{FederationError, SingleFederationError};
use crate::link::LinksMetadata;
use crate::schema::position::{
    CompositeTypeDefinitionPosition, DirectiveDefinitionPosition, EnumTypeDefinitionPosition,
    InputObjectTypeDefinitionPosition, InterfaceTypeDefinitionPosition,
    ObjectTypeDefinitionPosition, ScalarTypeDefinitionPosition, TypeDefinitionPosition,
    UnionTypeDefinitionPosition,
};
use apollo_compiler::schema::{ExtendedType, Name};
use apollo_compiler::validation::Valid;
use apollo_compiler::Schema;
use indexmap::IndexSet;
use referencer::Referencers;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::Arc;

pub(crate) mod position;
pub(crate) mod referencer;

#[derive(Debug)]
pub struct FederationSchema {
    schema: Schema,
    metadata: Option<LinksMetadata>,
    referencers: Referencers,
}

impl FederationSchema {
    pub(crate) fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Discard the Federation metadata and return the apollo-compiler schema.
    pub fn into_inner(self) -> Schema {
        self.schema
    }

    pub(crate) fn metadata(&self) -> Option<&LinksMetadata> {
        self.metadata.as_ref()
    }

    pub(crate) fn referencers(&self) -> &Referencers {
        &self.referencers
    }

    pub(crate) fn get_types(&self) -> impl Iterator<Item = TypeDefinitionPosition> + '_ {
        self.schema.types.iter().map(|(type_name, type_)| {
            let type_name = type_name.clone();
            match type_ {
                ExtendedType::Scalar(_) => ScalarTypeDefinitionPosition { type_name }.into(),
                ExtendedType::Object(_) => ObjectTypeDefinitionPosition { type_name }.into(),
                ExtendedType::Interface(_) => InterfaceTypeDefinitionPosition { type_name }.into(),
                ExtendedType::Union(_) => UnionTypeDefinitionPosition { type_name }.into(),
                ExtendedType::Enum(_) => EnumTypeDefinitionPosition { type_name }.into(),
                ExtendedType::InputObject(_) => {
                    InputObjectTypeDefinitionPosition { type_name }.into()
                }
            }
        })
    }

    pub(crate) fn get_directive_definitions(
        &self,
    ) -> impl Iterator<Item = DirectiveDefinitionPosition> + '_ {
        self.schema
            .directive_definitions
            .keys()
            .map(|name| DirectiveDefinitionPosition {
                directive_name: name.clone(),
            })
    }

    pub(crate) fn get_type(
        &self,
        type_name: Name,
    ) -> Result<TypeDefinitionPosition, FederationError> {
        let type_ =
            self.schema
                .types
                .get(&type_name)
                .ok_or_else(|| SingleFederationError::Internal {
                    message: format!("Schema has no type \"{}\"", type_name),
                })?;
        Ok(match type_ {
            ExtendedType::Scalar(_) => ScalarTypeDefinitionPosition { type_name }.into(),
            ExtendedType::Object(_) => ObjectTypeDefinitionPosition { type_name }.into(),
            ExtendedType::Interface(_) => InterfaceTypeDefinitionPosition { type_name }.into(),
            ExtendedType::Union(_) => UnionTypeDefinitionPosition { type_name }.into(),
            ExtendedType::Enum(_) => EnumTypeDefinitionPosition { type_name }.into(),
            ExtendedType::InputObject(_) => InputObjectTypeDefinitionPosition { type_name }.into(),
        })
    }

    pub(crate) fn try_get_type(&self, type_name: Name) -> Option<TypeDefinitionPosition> {
        self.get_type(type_name).ok()
    }

    pub(crate) fn possible_runtime_types(
        &self,
        composite_type_definition_position: CompositeTypeDefinitionPosition,
    ) -> Result<IndexSet<ObjectTypeDefinitionPosition>, FederationError> {
        Ok(match composite_type_definition_position {
            CompositeTypeDefinitionPosition::Object(pos) => IndexSet::from([pos]),
            CompositeTypeDefinitionPosition::Interface(pos) => self
                .referencers()
                .get_interface_type(&pos.type_name)?
                .object_types
                .clone(),
            CompositeTypeDefinitionPosition::Union(pos) => pos
                .get(self.schema())?
                .members
                .iter()
                .map(|t| ObjectTypeDefinitionPosition {
                    type_name: t.name.clone(),
                })
                .collect::<IndexSet<_>>(),
        })
    }

    pub(crate) fn validate(self) -> Result<ValidFederationSchema, FederationError> {
        let schema = self.schema.validate()?.into_inner();
        Ok(ValidFederationSchema(Arc::new(Valid::assume_valid(
            FederationSchema {
                schema,
                metadata: self.metadata,
                referencers: self.referencers,
            },
        ))))
    }

    pub(crate) fn get_directive_definition(
        &self,
        name: &Name,
    ) -> Option<DirectiveDefinitionPosition> {
        self.schema
            .directive_definitions
            .contains_key(name)
            .then(|| DirectiveDefinitionPosition {
                directive_name: name.clone(),
            })
    }
}

#[derive(Debug, Clone)]
pub struct ValidFederationSchema(pub(crate) Arc<Valid<FederationSchema>>);

impl ValidFederationSchema {
    pub fn new(schema: Valid<Schema>) -> Result<ValidFederationSchema, FederationError> {
        let schema = FederationSchema::new(schema.into_inner())?;
        Ok(ValidFederationSchema(Arc::new(Valid::assume_valid(schema))))
    }

    pub(crate) fn schema(&self) -> &Valid<Schema> {
        Valid::assume_valid_ref(&self.schema)
    }
}

impl Deref for ValidFederationSchema {
    type Target = FederationSchema;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Eq for ValidFederationSchema {}

impl PartialEq for ValidFederationSchema {
    fn eq(&self, other: &ValidFederationSchema) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Hash for ValidFederationSchema {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}
