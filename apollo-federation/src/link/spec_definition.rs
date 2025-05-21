use std::collections::BTreeMap;
use std::collections::btree_map::Keys;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::schema::DirectiveDefinition;
use apollo_compiler::schema::ExtendedType;

use crate::ensure;
use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::Link;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::schema::FederationSchema;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;

#[allow(dead_code)]
pub(crate) trait SpecDefinition {
    fn url(&self) -> &Url;

    fn directive_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>>;

    fn type_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>>;

    fn minimum_federation_version(&self) -> &Version;

    fn identity(&self) -> &Identity {
        &self.url().identity
    }

    fn version(&self) -> &Version {
        &self.url().version
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
                            "Unexpectedly could not find spec directive \"@{}\" in schema",
                            name
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
                            "Unexpectedly could not find spec type \"{}\" in schema",
                            name
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

    pub(crate) fn versions(&self) -> Keys<Version, T> {
        self.definitions.keys()
    }

    pub(crate) fn get_minimum_required_version(
        &'static self,
        federation_version: &Version,
    ) -> Option<&'static dyn SpecDefinition> {
        self.definitions
            .values()
            .find(|spec| federation_version.satisfies(spec.minimum_federation_version()))
            .map(|spec| spec as &dyn SpecDefinition)
    }
}
