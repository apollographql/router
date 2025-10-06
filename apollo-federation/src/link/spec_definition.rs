use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::btree_map::Keys;
use std::sync::Arc;
use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::schema::DirectiveDefinition;
use apollo_compiler::schema::ExtendedType;

use crate::AUTHENTICATED_VERSIONS;
use crate::CACHE_TAG_VERSIONS;
use crate::CONTEXT_VERSIONS;
use crate::COST_VERSIONS;
use crate::INACCESSIBLE_VERSIONS;
use crate::POLICY_VERSIONS;
use crate::REQUIRES_SCOPES_VERSIONS;
use crate::TAG_VERSIONS;
use crate::connectors::spec::CONNECT_VERSIONS;
use crate::ensure;
use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::Import;
use crate::link::Link;
use crate::link::Purpose;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::schema::FederationSchema;
use crate::schema::type_and_directive_specification::DirectiveCompositionSpecification;
use crate::schema::type_and_directive_specification::DirectiveSpecification;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;

pub(crate) trait SpecDefinition {
    fn url(&self) -> &Url;

    fn directive_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>>;

    fn type_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>>;

    fn minimum_federation_version(&self) -> &Version;

    fn purpose(&self) -> Option<Purpose>;

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
    ) -> Result<Option<Name>, FederationError> {
        let Some(link) = self.link_in_schema(schema)? else {
            return Ok(None);
        };
        Ok(Some(link.directive_name_in_schema(name_in_spec)))
    }

    fn type_name_in_schema(
        &self,
        schema: &FederationSchema,
        name_in_spec: &Name,
    ) -> Result<Option<Name>, FederationError> {
        let Some(link) = self.link_in_schema(schema)? else {
            return Ok(None);
        };
        Ok(Some(link.type_name_in_schema(name_in_spec)))
    }

    fn directive_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
        name_in_spec: &Name,
    ) -> Result<Option<&'schema Node<DirectiveDefinition>>, FederationError> {
        match self.directive_name_in_schema(schema, name_in_spec)? {
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

    fn type_definition<'schema>(
        &self,
        schema: &'schema FederationSchema,
        name_in_spec: &Name,
    ) -> Result<Option<&'schema ExtendedType>, FederationError> {
        match self.type_name_in_schema(schema, name_in_spec)? {
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

    fn link_in_schema(
        &self,
        schema: &FederationSchema,
    ) -> Result<Option<Arc<Link>>, FederationError> {
        let Some(metadata) = schema.metadata() else {
            return Ok(None);
        };
        Ok(metadata.for_identity(self.identity()))
    }

    fn to_string(&self) -> String {
        self.url().to_string()
    }

    fn add_elements_to_schema(&self, schema: &mut FederationSchema) -> Result<(), FederationError> {
        let link = self.link_in_schema(schema)?;
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

        if errors.errors.len() > 1 {
            Err(FederationError::MultipleFederationErrors(errors))
        } else if let Some(error) = errors.errors.pop() {
            Err(FederationError::SingleFederationError(error))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone)]
pub(crate) struct SpecDefinitions<T: SpecDefinition> {
    identity: Identity,
    definitions: BTreeMap<Version, T>,
}

impl<T: SpecDefinition> SpecDefinitions<T> {
    pub(crate) fn new(identity: Identity) -> Self {
        Self {
            identity,
            definitions: BTreeMap::new(),
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

pub(crate) struct SpecRegistry {
    definitions_by_url: HashMap<Url, &'static (dyn SpecDefinition + Sync)>,
    available_versions_by_identity: HashMap<Identity, BTreeSet<Version>>,
}

impl SpecRegistry {
    pub(crate) fn new() -> Self {
        Self {
            definitions_by_url: HashMap::new(),
            available_versions_by_identity: HashMap::new(),
        }
    }

    pub(crate) fn extend<T: SpecDefinition + Sync>(
        &mut self,
        definitions: &'static SpecDefinitions<T>,
    ) {
        for (v, spec) in definitions.iter() {
            self.definitions_by_url.insert(spec.url().clone(), spec);
            self.available_versions_by_identity
                .entry(spec.url().identity.clone())
                .or_default()
                .insert(v.clone());
        }
    }

    pub(crate) fn get_definition(&self, url: &Url) -> Option<&&(dyn SpecDefinition + Sync)> {
        self.definitions_by_url.get(url)
    }

    pub(crate) fn get_versions(&self, identity: &Identity) -> Option<&BTreeSet<Version>> {
        self.available_versions_by_identity.get(identity)
    }

    /// Generates the composition spec for an imported directive. Currently, this generates the
    /// entire spec, then loops over available directive specifications and clones the applicable
    /// directive. An alternative would be to mark everything as `Sync` and store them on the
    /// individual feature specs, but we have omitted this for now due to a non-trivial (~10%)
    /// increase in heap usage that affects query planning.
    pub(crate) fn get_composition_spec(
        &self,
        source: &Link,
        directive_import: &Import,
    ) -> Option<DirectiveCompositionSpecification> {
        let specs = self.get_definition(&source.url)?.directive_specs();
        let spec = specs
            .iter()
            .find(|s| *s.name() == directive_import.element)?;
        let directive_spec: DirectiveSpecification = spec.as_any().downcast_ref().cloned()?;
        directive_spec.composition
    }
}

pub(crate) static SPEC_REGISTRY: LazyLock<SpecRegistry> = LazyLock::new(|| {
    let mut registry = SpecRegistry::new();
    registry.extend(&AUTHENTICATED_VERSIONS);
    registry.extend(&CACHE_TAG_VERSIONS);
    registry.extend(&CONNECT_VERSIONS);
    registry.extend(&CONTEXT_VERSIONS);
    registry.extend(&COST_VERSIONS);
    registry.extend(&INACCESSIBLE_VERSIONS);
    registry.extend(&POLICY_VERSIONS);
    registry.extend(&REQUIRES_SCOPES_VERSIONS);
    registry.extend(&TAG_VERSIONS);
    registry
});
