use crate::error::{FederationError, SingleFederationError};
use crate::link::LinksMetadata;
use crate::schema::position::{
    CompositeTypeDefinitionPosition, EnumTypeDefinitionPosition, InputObjectTypeDefinitionPosition,
    InterfaceTypeDefinitionPosition, ObjectTypeDefinitionPosition, ScalarTypeDefinitionPosition,
    TypeDefinitionPosition, UnionTypeDefinitionPosition,
};
use apollo_compiler::schema::{ExtendedType, Name};
use apollo_compiler::validation::Valid;
use apollo_compiler::Schema;
use indexmap::IndexSet;
use referencer::Referencers;
use std::ops::Deref;

pub(crate) mod position;
pub(crate) mod referencer;

pub struct FederationSchema {
    schema: Schema,
    metadata: Option<LinksMetadata>,
    referencers: Referencers,
}

impl FederationSchema {
    pub(crate) fn schema(&self) -> &Schema {
        &self.schema
    }

    pub(crate) fn metadata(&self) -> &Option<LinksMetadata> {
        &self.metadata
    }

    pub(crate) fn referencers(&self) -> &Referencers {
        &self.referencers
    }

    pub(crate) fn get_types(&self) -> Vec<TypeDefinitionPosition> {
        self.schema
            .types
            .iter()
            .map(|(type_name, type_)| {
                let type_name = type_name.clone();
                match type_ {
                    ExtendedType::Scalar(_) => ScalarTypeDefinitionPosition { type_name }.into(),
                    ExtendedType::Object(_) => ObjectTypeDefinitionPosition { type_name }.into(),
                    ExtendedType::Interface(_) => {
                        InterfaceTypeDefinitionPosition { type_name }.into()
                    }
                    ExtendedType::Union(_) => UnionTypeDefinitionPosition { type_name }.into(),
                    ExtendedType::Enum(_) => EnumTypeDefinitionPosition { type_name }.into(),
                    ExtendedType::InputObject(_) => {
                        InputObjectTypeDefinitionPosition { type_name }.into()
                    }
                }
            })
            .collect()
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
        Ok(ValidFederationSchema(Valid::assume_valid(
            FederationSchema {
                schema,
                metadata: self.metadata,
                referencers: self.referencers,
            },
        )))
    }
}

pub struct ValidFederationSchema(pub(crate) Valid<FederationSchema>);

impl ValidFederationSchema {
    pub fn new(schema: Valid<Schema>) -> Result<ValidFederationSchema, FederationError> {
        let schema = FederationSchema::new(schema.into_inner())?;
        Ok(ValidFederationSchema(Valid::assume_valid(schema)))
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
