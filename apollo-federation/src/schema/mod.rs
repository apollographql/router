use std::hash::Hash;
use std::hash::Hasher;
use std::ops::Deref;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::validation::WithErrors;
use position::ObjectFieldDefinitionPosition;
use position::ObjectOrInterfaceTypeDefinitionPosition;
use position::TagDirectiveTargetPosition;
use referencer::Referencers;

use crate::bail;
use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::internal_error;
use crate::link::Link;
use crate::link::LinksMetadata;
use crate::link::cost_spec_definition;
use crate::link::cost_spec_definition::CostSpecDefinition;
use crate::link::federation_spec_definition::ContextDirectiveArguments;
use crate::link::federation_spec_definition::FEDERATION_ENTITY_TYPE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_FIELDSET_TYPE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_SERVICE_TYPE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::link::federation_spec_definition::FromContextDirectiveArguments;
use crate::link::federation_spec_definition::KeyDirectiveArguments;
use crate::link::federation_spec_definition::ProvidesDirectiveArguments;
use crate::link::federation_spec_definition::RequiresDirectiveArguments;
use crate::link::federation_spec_definition::TagDirectiveArguments;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
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
pub(crate) mod blueprint;
pub(crate) mod definitions;
pub(crate) mod field_set;
pub(crate) mod position;
pub(crate) mod referencer;
pub(crate) mod schema_upgrader;
pub(crate) mod subgraph_metadata;
pub(crate) mod validators;

pub(crate) fn compute_subgraph_metadata(
    schema: &FederationSchema,
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
#[derive(Clone, Debug)]
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
    pub(crate) fn get_types(&self) -> impl Iterator<Item = TypeDefinitionPosition> {
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
    ) -> impl Iterator<Item = DirectiveDefinitionPosition> {
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

    /// Return the possible runtime types for a definition.
    ///
    /// For a union, the possible runtime types are its members.
    /// For an interface, the possible runtime types are its implementers.
    ///
    /// Note this always allocates a set for the result. Avoid calling it frequently.
    pub(crate) fn possible_runtime_types(
        &self,
        composite_type_definition_position: CompositeTypeDefinitionPosition,
    ) -> Result<IndexSet<ObjectTypeDefinitionPosition>, FederationError> {
        Ok(match composite_type_definition_position {
            CompositeTypeDefinitionPosition::Object(pos) => IndexSet::from_iter([pos]),
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

    /// Similar to `Self::validate` but returns `self` as part of the error should it be needed by
    /// the caller
    #[allow(clippy::result_large_err)] // lint is accurate but this is not in a hot path
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
    // PORT_NOTE: Corresponds to `FederationMetadata.entityType` in JS
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

    // PORT_NOTE: Corresponds to `FederationMetadata.serviceType` in JS
    pub(crate) fn service_type(&self) -> Result<ObjectTypeDefinitionPosition, FederationError> {
        // Note: `_Service` type name can't be renamed.
        match self.schema.types.get(&FEDERATION_SERVICE_TYPE_NAME_IN_SPEC) {
            Some(ExtendedType::Object(_)) => Ok(ObjectTypeDefinitionPosition {
                type_name: FEDERATION_SERVICE_TYPE_NAME_IN_SPEC,
            }),
            Some(_) => bail!(
                "Unexpected type found for federation spec's `{spec_name}` type definition",
                spec_name = FEDERATION_SERVICE_TYPE_NAME_IN_SPEC,
            ),
            None => bail!(
                "Unexpected: type not found for federation spec's `{spec_name}`",
                spec_name = FEDERATION_SERVICE_TYPE_NAME_IN_SPEC,
            ),
        }
    }

    // PORT_NOTE: Corresponds to `FederationMetadata.isFed2Schema` in JS
    // This works even if the schema bootstrapping was not completed.
    pub(crate) fn is_fed_2(&self) -> bool {
        self.federation_link()
            .is_some_and(|link| link.url.version.satisfies(&Version { major: 2, minor: 0 }))
    }

    // PORT_NOTE: Corresponds to `FederationMetadata.federationFeature` in JS
    fn federation_link(&self) -> Option<&Arc<Link>> {
        self.metadata().and_then(|metadata| {
            metadata
                .by_identity
                .get(FederationSpecDefinition::latest().identity())
        })
    }

    // PORT_NOTE: Corresponds to `FederationMetadata.fieldSetType` in JS.
    pub(crate) fn field_set_type(&self) -> Result<ScalarTypeDefinitionPosition, FederationError> {
        let name_in_schema =
            self.federation_type_name_in_schema(FEDERATION_FIELDSET_TYPE_NAME_IN_SPEC)?;
        match self.schema.types.get(&name_in_schema) {
            Some(ExtendedType::Scalar(_)) => Ok(ScalarTypeDefinitionPosition {
                type_name: name_in_schema,
            }),
            Some(_) => bail!(
                "Unexpected type found for federation spec's `{name_in_schema}` type definition"
            ),
            None => {
                bail!("Unexpected: type not found for federation spec's `{name_in_schema}`")
            }
        }
    }

    // PORT_NOTE: Corresponds to `FederationMetadata.federationTypeNameInSchema` in JS.
    // Note: Unfortunately, this overlaps with `ValidFederationSchema`'s
    //       `federation_type_name_in_schema` method. This method was added because it's used
    //       during composition before `ValidFederationSchema` is created.
    pub(crate) fn federation_type_name_in_schema(
        &self,
        name: Name,
    ) -> Result<Name, FederationError> {
        // Currently, the types used to define the federation operations, that is _Any, _Entity and
        // _Service, are not considered part of the federation spec, and are instead hardcoded to
        // the names above. The reason being that there is no way to maintain backward
        // compatibility with fed2 if we were to add those to the federation spec without requiring
        // users to add those types to their @link `import`, and that wouldn't be a good user
        // experience (because most users don't really know what those types are/do). And so we
        // special case it.
        if name.starts_with('_') {
            return Ok(name);
        }

        if self.is_fed_2() {
            let Some(links) = self.metadata() else {
                bail!("Schema should be a core schema")
            };
            let Some(federation_link) = links
                .by_identity
                .get(FederationSpecDefinition::latest().identity())
            else {
                bail!("Schema should have the latest federation link")
            };
            Ok(federation_link.type_name_in_schema(&name))
        } else {
            // The only type here so far is the the `FieldSet` one. And in fed1, it's called `_FieldSet`, so ...
            Name::new(&format!("_{name}"))
                .map_err(|e| internal_error!("Invalid name `_{name}`: {e}"))
        }
    }

    pub(crate) fn context_directive_applications(
        &self,
    ) -> FallibleDirectiveIterator<ContextDirective> {
        let federation_spec = get_federation_spec_definition_from_subgraph(self)?;
        let context_directive_definition = federation_spec.context_directive_definition(self)?;
        let context_directive_referencers = self
            .referencers()
            .get_directive(&context_directive_definition.name)?;

        let mut applications = Vec::new();
        for interface_type_position in &context_directive_referencers.interface_types {
            match interface_type_position.get(self.schema()) {
                Ok(interface_type) => {
                    let directives = &interface_type.directives;
                    for directive in directives.get_all(&context_directive_definition.name) {
                        let arguments = federation_spec.context_directive_arguments(directive);
                        applications.push(arguments.map(|args| ContextDirective {
                            arguments: args,
                            target: interface_type_position.clone().into(),
                        }));
                    }
                }
                Err(error) => applications.push(Err(error.into())),
            }
        }
        for object_type_position in &context_directive_referencers.object_types {
            match object_type_position.get(self.schema()) {
                Ok(object_type) => {
                    let directives = &object_type.directives;
                    for directive in directives.get_all(&context_directive_definition.name) {
                        let arguments = federation_spec.context_directive_arguments(directive);
                        applications.push(arguments.map(|args| ContextDirective {
                            arguments: args,
                            target: object_type_position.clone().into(),
                        }));
                    }
                }
                Err(error) => applications.push(Err(error.into())),
            }
        }
        for union_type_position in &context_directive_referencers.union_types {
            match union_type_position.get(self.schema()) {
                Ok(union_type) => {
                    let directives = &union_type.directives;
                    for directive in directives.get_all(&context_directive_definition.name) {
                        let arguments = federation_spec.context_directive_arguments(directive);
                        applications.push(arguments.map(|args| ContextDirective {
                            arguments: args,
                            target: union_type_position.clone().into(),
                        }));
                    }
                }
                Err(error) => applications.push(Err(error.into())),
            }
        }
        Ok(applications)
    }

    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn from_context_directive_applications(
        &self,
    ) -> FallibleDirectiveIterator<FromContextDirective> {
        let federation_spec = get_federation_spec_definition_from_subgraph(self)?;
        let from_context_directive_definition =
            federation_spec.from_context_directive_definition(self)?;
        let from_context_directive_referencers = self
            .referencers()
            .get_directive(&from_context_directive_definition.name)?;

        let mut applications = Vec::new();
        for interface_field_argument_position in
            &from_context_directive_referencers.interface_field_arguments
        {
            match interface_field_argument_position.get(self.schema()) {
                Ok(interface_field_argument) => {
                    let directives = &interface_field_argument.directives;
                    for directive in directives.get_all(&from_context_directive_definition.name) {
                        let arguments = federation_spec.from_context_directive_arguments(directive);
                        applications
                            .push(arguments.map(|args| FromContextDirective { arguments: args }));
                    }
                }
                Err(error) => applications.push(Err(error.into())),
            }
        }
        for object_field_argument_position in
            &from_context_directive_referencers.object_field_arguments
        {
            match object_field_argument_position.get(self.schema()) {
                Ok(object_field_argument) => {
                    let directives = &object_field_argument.directives;
                    for directive in directives.get_all(&from_context_directive_definition.name) {
                        let arguments = federation_spec.from_context_directive_arguments(directive);
                        applications
                            .push(arguments.map(|args| FromContextDirective { arguments: args }));
                    }
                }
                Err(error) => applications.push(Err(error.into())),
            }
        }
        Ok(applications)
    }

    pub(crate) fn key_directive_applications(&self) -> FallibleDirectiveIterator<KeyDirective> {
        let federation_spec = get_federation_spec_definition_from_subgraph(self)?;
        let key_directive_definition = federation_spec.key_directive_definition(self)?;
        let key_directive_referencers = self
            .referencers()
            .get_directive(&key_directive_definition.name)?;

        let mut applications: Vec<Result<KeyDirective, FederationError>> = Vec::new();
        for object_type_position in &key_directive_referencers.object_types {
            match object_type_position.get(self.schema()) {
                Ok(object_type) => {
                    let directives = &object_type.directives;
                    for directive in directives.get_all(&key_directive_definition.name) {
                        let arguments = federation_spec.key_directive_arguments(directive);
                        applications.push(arguments.map(|args| KeyDirective {
                            arguments: args,
                            schema_directive: directive,
                            sibling_directives: directives,
                            target: object_type_position.clone().into(),
                        }));
                    }
                }
                Err(error) => applications.push(Err(error.into())),
            }
        }
        for interface_type_position in &key_directive_referencers.interface_types {
            match interface_type_position.get(self.schema()) {
                Ok(interface_type) => {
                    let directives = &interface_type.directives;
                    for directive in directives.get_all(&key_directive_definition.name) {
                        let arguments = federation_spec.key_directive_arguments(directive);
                        applications.push(arguments.map(|args| KeyDirective {
                            arguments: args,
                            schema_directive: directive,
                            sibling_directives: directives,
                            target: interface_type_position.clone().into(),
                        }));
                    }
                }
                Err(error) => applications.push(Err(error.into())),
            }
        }
        Ok(applications)
    }

    pub(crate) fn provides_directive_applications(
        &self,
    ) -> FallibleDirectiveIterator<ProvidesDirective> {
        let federation_spec = get_federation_spec_definition_from_subgraph(self)?;
        let provides_directive_definition = federation_spec.provides_directive_definition(self)?;
        let provides_directive_referencers = self
            .referencers()
            .get_directive(&provides_directive_definition.name)?;

        let mut applications: Vec<Result<ProvidesDirective, FederationError>> = Vec::new();
        for field_definition_position in &provides_directive_referencers.object_fields {
            match field_definition_position.get(self.schema()) {
                Ok(field_definition) => {
                    let directives = &field_definition.directives;
                    for provides_directive_application in
                        directives.get_all(&provides_directive_definition.name)
                    {
                        let arguments = federation_spec
                            .provides_directive_arguments(provides_directive_application);
                        applications.push(arguments.map(|args| ProvidesDirective {
                            arguments: args,
                            target: field_definition_position,
                            target_return_type: field_definition.ty.inner_named_type(),
                        }));
                    }
                }
                Err(error) => applications.push(Err(error.into())),
            }
        }
        Ok(applications)
    }

    pub(crate) fn requires_directive_applications(
        &self,
    ) -> FallibleDirectiveIterator<RequiresDirective> {
        let federation_spec = get_federation_spec_definition_from_subgraph(self)?;
        let requires_directive_definition = federation_spec.requires_directive_definition(self)?;
        let requires_directive_referencers = self
            .referencers()
            .get_directive(&requires_directive_definition.name)?;

        let mut applications = Vec::new();
        for field_definition_position in &requires_directive_referencers.object_fields {
            match field_definition_position.get(self.schema()) {
                Ok(field_definition) => {
                    let directives = &field_definition.directives;
                    for provides_directive_application in
                        directives.get_all(&requires_directive_definition.name)
                    {
                        let arguments = federation_spec
                            .requires_directive_arguments(provides_directive_application);
                        applications.push(arguments.map(|args| RequiresDirective {
                            arguments: args,
                            target: field_definition_position,
                        }));
                    }
                }
                Err(error) => applications.push(Err(error.into())),
            }
        }
        Ok(applications)
    }

    // TODO: This currently only returns targets for object_fields and interface_fields
    pub(crate) fn tag_directive_applications(&self) -> FallibleDirectiveIterator<TagDirective> {
        let federation_spec = get_federation_spec_definition_from_subgraph(self)?;
        let tag_directive_definition = federation_spec.tag_directive_definition(self)?;
        let tag_directive_referencers = self
            .referencers()
            .get_directive(&tag_directive_definition.name)?;

        let mut applications = Vec::new();
        for field_definition_position in &tag_directive_referencers.object_fields {
            match field_definition_position.get(self.schema()) {
                Ok(field_definition) => {
                    let directives = &field_definition.directives;
                    for tag_directive_application in
                        directives.get_all(&tag_directive_definition.name)
                    {
                        let arguments =
                            federation_spec.tag_directive_arguments(tag_directive_application);
                        applications.push(arguments.map(|args| TagDirective {
                            arguments: args,
                            target: TagDirectiveTargetPosition::ObjectField(
                                field_definition_position.clone(),
                            ),
                            directive: tag_directive_application,
                        }));
                    }
                }
                Err(error) => applications.push(Err(error.into())),
            }
        }
        for field_definition_position in &tag_directive_referencers.interface_fields {
            match field_definition_position.get(self.schema()) {
                Ok(field_definition) => {
                    let directives = &field_definition.directives;
                    for tag_directive_application in
                        directives.get_all(&tag_directive_definition.name)
                    {
                        let arguments =
                            federation_spec.tag_directive_arguments(tag_directive_application);
                        applications.push(arguments.map(|args| TagDirective {
                            arguments: args,
                            target: TagDirectiveTargetPosition::InterfaceField(
                                field_definition_position.clone(),
                            ),
                            directive: tag_directive_application,
                        }));
                    }
                }
                Err(error) => applications.push(Err(error.into())),
            }
        }
        Ok(applications)
    }

    pub(crate) fn list_size_directive_applications(
        &self,
    ) -> FallibleDirectiveIterator<ListSizeDirective> {
        let Some(list_size_directive_name) = CostSpecDefinition::list_size_directive_name(self)?
        else {
            return Ok(Vec::new());
        };
        let Ok(list_size_directive_referencers) = self
            .referencers()
            .get_directive(list_size_directive_name.as_str())
        else {
            return Ok(Vec::new());
        };

        let mut applications = Vec::new();
        for field_definition_position in &list_size_directive_referencers.object_fields {
            let field_definition = field_definition_position.get(self.schema())?;
            match CostSpecDefinition::list_size_directive_from_field_definition(
                self,
                field_definition,
            ) {
                Ok(Some(list_size_directive)) => {
                    applications.push(Ok(ListSizeDirective {
                        directive: list_size_directive,
                        target: field_definition,
                    }));
                }
                Ok(None) => {
                    // No listSize directive found, continue
                }
                Err(error) => {
                    applications.push(Err(error));
                }
            }
        }
        for field_definition_position in &list_size_directive_referencers.interface_fields {
            let field_definition = field_definition_position.get(self.schema())?;
            match CostSpecDefinition::list_size_directive_from_field_definition(
                self,
                field_definition,
            ) {
                Ok(Some(list_size_directive)) => {
                    applications.push(Ok(ListSizeDirective {
                        directive: list_size_directive,
                        target: field_definition,
                    }));
                }
                Ok(None) => {
                    // No listSize directive found, continue
                }
                Err(error) => {
                    applications.push(Err(error));
                }
            }
        }
        Ok(applications)
    }

    pub(crate) fn is_interface(&self, type_name: &Name) -> bool {
        self.referencers().interface_types.contains_key(type_name)
    }
}

type FallibleDirectiveIterator<D> = Result<Vec<Result<D, FederationError>>, FederationError>;

pub(crate) struct ContextDirective<'schema> {
    /// The parsed arguments of this `@context` application
    arguments: ContextDirectiveArguments<'schema>,
    /// The schema position to which this directive is applied
    target: CompositeTypeDefinitionPosition,
}

pub(crate) struct FromContextDirective<'schema> {
    /// The parsed arguments of this `@fromContext` application
    arguments: FromContextDirectiveArguments<'schema>,
}

pub(crate) struct KeyDirective<'schema> {
    /// The parsed arguments of this `@key` application
    arguments: KeyDirectiveArguments<'schema>,
    /// The original `Directive` instance from the AST with unparsed arguments
    schema_directive: &'schema apollo_compiler::schema::Component<Directive>,
    /// The `DirectiveList` containing all directives applied to the target position, including this one
    sibling_directives: &'schema apollo_compiler::schema::DirectiveList,
    /// The schema position to which this directive is applied
    target: ObjectOrInterfaceTypeDefinitionPosition,
}

impl HasFields for KeyDirective<'_> {
    fn fields(&self) -> &str {
        self.arguments.fields
    }

    fn target_type(&self) -> &Name {
        self.target.type_name()
    }
}

impl KeyDirective<'_> {
    pub(crate) fn target(&self) -> &ObjectOrInterfaceTypeDefinitionPosition {
        &self.target
    }
}

pub(crate) struct ListSizeDirective<'schema> {
    /// The parsed directive
    directive: cost_spec_definition::ListSizeDirective,
    /// The schema position to which this directive is applied
    target: &'schema FieldDefinition,
}

pub(crate) struct ProvidesDirective<'schema> {
    /// The parsed arguments of this `@provides` application
    arguments: ProvidesDirectiveArguments<'schema>,
    /// The schema position to which this directive is applied
    target: &'schema ObjectFieldDefinitionPosition,
    /// The return type of the target field
    target_return_type: &'schema Name,
}

impl HasFields for ProvidesDirective<'_> {
    /// The string representation of the field set
    fn fields(&self) -> &str {
        self.arguments.fields
    }

    /// The type from which the field set selects
    fn target_type(&self) -> &Name {
        self.target_return_type
    }
}

pub(crate) struct RequiresDirective<'schema> {
    /// The parsed arguments of this `@requires` application
    arguments: RequiresDirectiveArguments<'schema>,
    /// The schema position to which this directive is applied
    target: &'schema ObjectFieldDefinitionPosition,
}

impl HasFields for RequiresDirective<'_> {
    fn fields(&self) -> &str {
        self.arguments.fields
    }

    fn target_type(&self) -> &Name {
        &self.target.type_name
    }
}

pub(crate) struct TagDirective<'schema> {
    /// The parsed arguments of this `@tag` application
    arguments: TagDirectiveArguments<'schema>,
    /// The schema position to which this directive is applied
    target: TagDirectiveTargetPosition, // TODO: Make this a reference
    /// Reference to the directive in the schema
    directive: &'schema Node<Directive>,
}

pub(crate) trait HasFields {
    fn fields(&self) -> &str;
    fn target_type(&self) -> &Name;

    fn parse_fields(&self, schema: &Schema) -> Result<Valid<FieldSet>, WithErrors<FieldSet>> {
        FieldSet::parse_and_validate(
            Valid::assume_valid_ref(schema),
            self.target_type().clone(),
            self.fields(),
            "field_set.graphql",
        )
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

    /// Construct a ValidFederationSchema by assuming the given FederationSchema is valid.
    #[allow(clippy::result_large_err)] // lint is accurate but this is not in a hot path
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
        // The reason being that there is no way to maintain backward compatibility with fed2 if we were to add
        // those to the federation spec without requiring users to add those types to their @link `import`,
        // and that wouldn't be a good user experience (because most users don't really know what those types
        // are/do). And so we special case it.
        if name.starts_with('_') {
            return Ok(name);
        }

        // TODO for composition: this otherwise needs to check for a type name in schema based
        // on the latest federation version.
        // This code path is not hit during planning.
        Err(FederationError::internal(
            "typename should have been looked in a federation feature",
        ))
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
