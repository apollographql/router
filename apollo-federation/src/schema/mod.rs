use std::hash::Hash;
use std::hash::Hasher;
use std::ops::Deref;
use std::sync::Arc;

use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::Name;
use apollo_compiler::validation::Valid;
use apollo_compiler::Schema;
use indexmap::IndexSet;
use referencer::Referencers;

use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::link::federation_spec_definition::FEDERATION_ENTITY_TYPE_NAME_IN_SPEC;
use crate::link::LinksMetadata;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::DirectiveDefinitionPosition;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::InputObjectTypeDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;
use crate::schema::position::UnionTypeDefinitionPosition;
use crate::schema::subgraph_metadata::SubgraphMetadata;

pub(crate) mod argument_composition_strategies;
pub(crate) mod definitions;
pub(crate) mod field_set;
pub(crate) mod position;
pub(crate) mod referencer;
pub(crate) mod subgraph_metadata;
pub use position::ObjectFieldDefinitionPosition;
pub use position::ObjectOrInterfaceFieldDefinitionPosition;
pub use position::ObjectOrInterfaceFieldDirectivePosition;

fn compute_subgraph_metadata(
    schema: &Valid<FederationSchema>,
) -> Result<Option<SubgraphMetadata>, FederationError> {
    Ok(
        if let Ok(federation_spec_definition) = get_federation_spec_definition_from_subgraph(schema)
        {
            Some(SubgraphMetadata::new(schema, federation_spec_definition)?)
        } else {
            None
        },
    )
}
pub(crate) mod type_and_directive_specification;

/// A GraphQL schema with federation data.
#[derive(Debug)]
pub struct FederationSchema {
    schema: Schema,
    referencers: Referencers,
    links_metadata: Option<Box<LinksMetadata>>,
    /// This is only populated for valid subgraphs, and can only be accessed if you have a
    /// `ValidFederationSchema`.
    subgraph_metadata: Option<Box<SubgraphMetadata>>,
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
        self.links_metadata.as_deref()
    }

    pub(crate) fn referencers(&self) -> &Referencers {
        &self.referencers
    }

    /// Returns all the types in the schema, minus builtins.
    pub(crate) fn get_types(&self) -> impl Iterator<Item = TypeDefinitionPosition> + '_ {
        self.schema
            .types
            .iter()
            .filter(|(_, ty)| !ty.is_built_in())
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
        self.validate_or_return_self().map_err(|e| e.1)
    }

    /// Similar to `Self::validate` but returns `self` as part of the error should it be needed by
    /// the caller
    pub(crate) fn validate_or_return_self(
        mut self,
    ) -> Result<ValidFederationSchema, (Self, FederationError)> {
        let schema = match self.schema.validate() {
            Ok(schema) => schema.into_inner(),
            Err(e) => {
                self.schema = e.partial;
                return Err((self, e.errors.into()));
            }
        };
        ValidFederationSchema::new_assume_valid(FederationSchema { schema, ..self })
    }

    pub(crate) fn assume_valid(self) -> Result<ValidFederationSchema, FederationError> {
        ValidFederationSchema::new_assume_valid(self).map_err(|(_schema, error)| error)
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

    /// Note that a subgraph may have no "entities" and so no `_Entity` type.
    pub(crate) fn entity_type(
        &self,
    ) -> Result<Option<UnionTypeDefinitionPosition>, FederationError> {
        // Note that the _Entity type is special in that:
        // 1. Spec renaming doesn't take place for it (there's no prefixing or importing needed),
        //    in order to maintain backwards compatibility with Fed 1.
        // 2. Its presence is optional; if absent, it means the subgraph has no resolvable keys.
        match self.schema.types.get(&FEDERATION_ENTITY_TYPE_NAME_IN_SPEC) {
            Some(ExtendedType::Union(_)) => Ok(Some(UnionTypeDefinitionPosition {
                type_name: FEDERATION_ENTITY_TYPE_NAME_IN_SPEC,
            })),
            Some(_) => Err(FederationError::internal(format!(
                "Unexpectedly found non-union for federation spec's `{}` type definition",
                FEDERATION_ENTITY_TYPE_NAME_IN_SPEC
            ))),
            None => Ok(None),
        }
    }
}

/// A GraphQL schema with federation data that is known to be valid, and cheap to clone.
#[derive(Clone)]
pub struct ValidFederationSchema {
    schema: Arc<Valid<FederationSchema>>,
}

impl ValidFederationSchema {
    pub fn new(schema: Valid<Schema>) -> Result<ValidFederationSchema, FederationError> {
        let schema = FederationSchema::new(schema.into_inner())?;

        Self::new_assume_valid(schema).map_err(|(_schema, error)| error)
    }

    pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.schema, &other.schema)
    }

    /// Construct a ValidFederationSchema by assuming the given FederationSchema is valid.
    fn new_assume_valid(
        mut schema: FederationSchema,
    ) -> Result<ValidFederationSchema, (FederationSchema, FederationError)> {
        // Populating subgraph metadata requires a mutable FederationSchema, while computing the subgraph
        // metadata requires a valid FederationSchema. Since valid schemas are immutable, we have
        // to jump through some hoops here. We already assume that `schema` is valid GraphQL, so we
        // can temporarily create a `&Valid<FederationSchema>` to compute subgraph metadata, drop
        // that reference to populate the metadata, and finally move the finished FederationSchema into
        // the ValidFederationSchema instance.
        let valid_schema = Valid::assume_valid_ref(&schema);
        let subgraph_metadata = match compute_subgraph_metadata(valid_schema) {
            Ok(metadata) => metadata.map(Box::new),
            Err(err) => return Err((schema, err)),
        };
        schema.subgraph_metadata = subgraph_metadata;

        let schema = Arc::new(Valid::assume_valid(schema));
        Ok(ValidFederationSchema { schema })
    }

    /// Access the GraphQL schema.
    pub fn schema(&self) -> &Valid<Schema> {
        Valid::assume_valid_ref(&self.schema.schema)
    }

    /// Returns subgraph-specific metadata.
    ///
    /// Returns `None` for supergraph schemas.
    pub(crate) fn subgraph_metadata(&self) -> Option<&SubgraphMetadata> {
        self.schema.subgraph_metadata.as_deref()
    }

    pub(crate) fn federation_type_name_in_schema(
        &self,
        name: Name,
    ) -> Result<Name, FederationError> {
        // Currently, the types used to define the federation operations, that is _Any, _Entity and _Service,
        // are not considered part of the federation spec, and are instead hardcoded to the names above.
        // The reason being that there is no way to maintain backward compatbility with fed2 if we were to add
        // those to the federation spec without requiring users to add those types to their @link `import`,
        // and that wouldn't be a good user experience (because most users don't really know what those types
        // are/do). And so we special case it.
        if name.starts_with('_') {
            return Ok(name);
        }

        // TODO: this otherwise needs to check for a type name in schema based
        // on the latest federation version.
        // FED-311
        Err(SingleFederationError::Internal {
            message: String::from("typename should have been looked in a federation feature"),
        }
        .into())
    }

    pub(crate) fn is_interface_object_type(
        &self,
        type_definition_position: TypeDefinitionPosition,
    ) -> Result<bool, FederationError> {
        let Some(subgraph_metadata) = &self.subgraph_metadata else {
            return Ok(false);
        };
        let Some(interface_object_directive_definition) = subgraph_metadata
            .federation_spec_definition()
            .interface_object_directive_definition(self)?
        else {
            return Ok(false);
        };
        match type_definition_position {
            TypeDefinitionPosition::Object(type_) => Ok(type_
                .get(self.schema())?
                .directives
                .has(&interface_object_directive_definition.name)),
            _ => Ok(false),
        }
    }
}

impl Deref for ValidFederationSchema {
    type Target = FederationSchema;

    fn deref(&self) -> &Self::Target {
        &self.schema
    }
}

impl Eq for ValidFederationSchema {}

impl PartialEq for ValidFederationSchema {
    fn eq(&self, other: &ValidFederationSchema) -> bool {
        Arc::ptr_eq(&self.schema, &other.schema)
    }
}

impl Hash for ValidFederationSchema {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.schema).hash(state);
    }
}

impl std::fmt::Debug for ValidFederationSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ValidFederationSchema @ {:?}", Arc::as_ptr(&self.schema))
    }
}
