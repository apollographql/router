use std::collections::BTreeMap;
use std::collections::btree_map::Keys;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::collections::HashSet;
use apollo_compiler::schema::DirectiveDefinition;
use apollo_compiler::schema::ExtendedType;
use itertools::Itertools;

use crate::ensure;
use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::ElementName;
use crate::link::Link;
use crate::link::Purpose;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::schema::FederationSchema;
use crate::schema::type_and_directive_specification::DirectiveSpecification;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;

pub(crate) trait SpecDefinition {
    fn url(&self) -> &Url;

    fn directive_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>>;

    fn type_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>>;

    fn minimum_federation_version(&self) -> &Version;

    fn purpose(&self) -> Option<Purpose>;

    /// Whether applications from this spec are persisted through `@join__directive`.
    ///
    /// Most specs derive this from their directive composition metadata. Specifications with
    /// legacy custom composition may override this while they migrate to generic composition.
    fn uses_join_directive(&self) -> bool {
        self.directive_specs().into_iter().any(|specification| {
            specification
                .as_any()
                .downcast_ref::<DirectiveSpecification>()
                .and_then(|directive| directive.composition.as_ref())
                .is_some_and(|composition| composition.use_join_directive)
        })
    }

    fn identity(&self) -> &Identity {
        &self.url().identity
    }

    fn version(&self) -> &Version {
        &self.url().version
    }

    fn is_spec_directive_name(
        &self,
        schema: &FederationSchema,
        name_in_schema: &Name,
    ) -> Result<bool, FederationError> {
        let Some(metadata) = schema.metadata() else {
            return Err(SingleFederationError::Internal {
                message: "Schema is not a core schema (add @link first)".to_owned(),
            }
            .into());
        };
        Ok(metadata
            .source_link_of_directive(name_in_schema)
            .map(|e| e.link.url.identity == *self.identity())
            .unwrap_or(false))
    }

    fn is_spec_type_name(
        &self,
        schema: &FederationSchema,
        name_in_schema: &Name,
    ) -> Result<bool, FederationError> {
        let Some(metadata) = schema.metadata() else {
            return Err(SingleFederationError::Internal {
                message: "Schema is not a core schema (add @link first)".to_owned(),
            }
            .into());
        };
        Ok(metadata
            .source_link_of_type(name_in_schema)
            .map(|e| e.link.url.identity == *self.identity())
            .unwrap_or(false))
    }

    fn directive_name_in_schema(
        &self,
        schema: &FederationSchema,
        name_in_spec: &Name,
    ) -> Option<Name> {
        let link = self.link_in_schema(schema)?;
        Some(link.directive_name_in_schema(name_in_spec))
    }

    fn type_name_in_schema(&self, schema: &FederationSchema, name_in_spec: &Name) -> Option<Name> {
        let link = self.link_in_schema(schema)?;
        Some(link.type_name_in_schema(name_in_spec))
    }

    fn directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
        name_in_spec: &Name,
    ) -> Result<Option<&'schema Node<DirectiveDefinition>>, FederationError> {
        match self.directive_name_in_schema(schema, name_in_spec) {
            Some(name) => schema
                .schema()
                .directive_definitions
                .get(&name)
                .ok_or_else(|| {
                    SingleFederationError::Internal {
                        message: format!(
                            "Unexpectedly could not find spec directive \"@{name}\" in schema"
                        ),
                    }
                    .into()
                })
                .map(Some),
            None => Ok(None),
        }
    }

    fn try_directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
        name_in_spec: &Name,
    ) -> Option<&'schema Node<DirectiveDefinition>> {
        match self.directive_name_in_schema(schema, name_in_spec) {
            Some(name) => schema.schema().directive_definitions.get(&name),
            None => None,
        }
    }

    fn type_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
        name_in_spec: &Name,
    ) -> Result<Option<&'schema ExtendedType>, FederationError> {
        match self.type_name_in_schema(schema, name_in_spec) {
            Some(name) => schema
                .schema()
                .types
                .get(&name)
                .ok_or_else(|| {
                    SingleFederationError::Internal {
                        message: format!(
                            "Unexpectedly could not find spec type \"{name}\" in schema"
                        ),
                    }
                    .into()
                })
                .map(Some),
            None => Ok(None),
        }
    }

    fn link_in_schema(&self, schema: &FederationSchema) -> Option<Arc<Link>> {
        let metadata = schema.metadata()?;
        metadata.for_identity(self.identity())
    }

    fn to_string(&self) -> String {
        self.url().to_string()
    }

    fn add_elements_to_schema(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let link = self.link_in_schema(schema);
        ensure!(
            link.is_some(),
            "The {self_url} specification should have been added to the schema before this is called",
            self_url = self.url()
        );
        let mut errors = MultipleFederationErrors { errors: vec![] };
        for type_spec in self.type_specs() {
            if let Err(err) = type_spec.check_or_add(schema, link.as_ref()) {
                errors.push(err);
            }
        }

        for directive_spec in self.directive_specs() {
            if let Err(err) = directive_spec.check_or_add(schema, link.as_ref()) {
                errors.push(err);
            }
        }

        match errors.errors.as_slice() {
            [] => Ok(()),
            [error] => Err(FederationError::SingleFederationError(error.clone())),
            _ => Err(FederationError::MultipleFederationErrors(errors)),
        }
    }

    fn all_element_names(&self) -> Box<dyn Iterator<Item = ElementName>> {
        Box::new(
            self.type_specs()
                .into_iter()
                .map(|spec| ElementName {
                    name: spec.name().clone(),
                    is_directive: false,
                })
                .chain(self.directive_specs().into_iter().map(|spec| ElementName {
                    name: spec.name().clone(),
                    is_directive: true,
                })),
        )
    }
}

#[derive(Clone)]
pub(crate) struct SpecDefinitions<T: SpecDefinition> {
    identity: Identity,
    definitions: BTreeMap<Version, T>,
    preview_versions: HashSet<Version>,
}

impl<T: SpecDefinition> SpecDefinitions<T> {
    pub(crate) fn new(identity: Identity) -> Self {
        Self {
            identity,
            definitions: BTreeMap::new(),
            preview_versions: Default::default(),
        }
    }

    pub(crate) fn add(&mut self, definition: T) {
        assert_eq!(
            *definition.identity(),
            self.identity,
            "Cannot add definition for {} to the versions of definitions for {}",
            definition.to_string(),
            self.identity
        );
        if self.definitions.contains_key(definition.version()) {
            return;
        }
        self.definitions
            .insert(definition.version().clone(), definition);
    }

    pub(crate) fn add_preview(&mut self, definition: T) {
        let preview_version = definition.version().clone();
        self.add(definition);
        self.preview_versions.insert(preview_version);
    }

    pub(crate) fn find(&self, requested: &Version) -> Option<&T> {
        self.definitions.get(requested)
    }

    pub(crate) fn versions(&self) -> Keys<'_, Version, T> {
        self.definitions.keys()
    }

    pub(crate) fn latest(&self) -> &T {
        self.definitions
            .last_key_value()
            .expect("There should always be at least one version defined")
            .1
    }

    /// Like [`SpecDefinitions::latest`], but skips versions marked as preview. Used by
    /// [`Merger::add_join_directive_directives`] in the composition merger to avoid
    /// stamping a preview spec version (e.g. connect/v0.4) into the
    /// supergraph `@link` when no subgraph explicitly uses it. Falls back
    /// to latest if all versions are preview.
    pub(crate) fn latest_non_preview(&self) -> &T {
        self.definitions
            .iter()
            .rev()
            .find_or_first(|(v, _)| !self.preview_versions.contains(v))
            .expect("There should always be at least one non-preview version defined")
            .1
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Version, &T)> {
        self.definitions.iter()
    }

    pub(crate) fn get_maximum_allowed_version(
        &'static self,
        federation_version: &Version,
    ) -> Option<&'static T> {
        self.definitions
            .values()
            .rev()
            .find(|spec| federation_version.satisfies(spec.minimum_federation_version()))
    }

    pub(crate) fn get_minimum_required_version(
        &'static self,
        federation_version: &Version,
    ) -> Option<&'static T> {
        self.definitions
            .values()
            .find(|spec| federation_version.satisfies(spec.minimum_federation_version()))
    }

    pub(crate) fn get_dyn_minimum_required_version(
        &'static self,
        federation_version: &Version,
    ) -> Option<&'static dyn SpecDefinition> {
        self.get_minimum_required_version(federation_version)
            .map(|spec| spec as &dyn SpecDefinition)
    }
}
