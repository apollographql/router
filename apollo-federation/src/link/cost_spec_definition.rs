use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::EnumType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::schema::ScalarType;
use apollo_compiler::Name;
use apollo_compiler::Node;
use lazy_static::lazy_static;

use crate::error::FederationError;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;
use crate::schema::FederationSchema;

use super::federation_spec_definition::get_federation_spec_definition_from_subgraph;

pub(crate) const COST_DIRECTIVE_NAME_IN_SPEC: Name = name!("cost");
pub(crate) const COST_DIRECTIVE_NAME_DEFAULT: Name = name!("federation__cost");

pub(crate) const LIST_SIZE_DIRECTIVE_NAME_IN_SPEC: Name = name!("listSize");
pub(crate) const LIST_SIZE_DIRECTIVE_NAME_DEFAULT: Name = name!("federation__listSize");

#[derive(Clone)]
pub struct CostSpecDefinition {
    url: Url,
    minimum_federation_version: Option<Version>,
}

macro_rules! propagate_demand_control_directives {
    ($func_name:ident, $directives_ty:ty, $wrap_ty:expr) => {
        pub(crate) fn $func_name(
            &self,
            subgraph_schema: &FederationSchema,
            source: &$directives_ty,
            dest: &mut $directives_ty,
        ) -> Result<(), FederationError> {
            let cost_directive = self
                .directive_name_in_schema(subgraph_schema, &COST_DIRECTIVE_NAME_IN_SPEC)?
                .and_then(|name| source.get(name.as_str()));
            if let Some(cost_directive) = cost_directive {
                dest.push($wrap_ty(self.cost_directive(
                    subgraph_schema,
                    cost_directive.arguments.clone(),
                )?));
            }

            let list_size_directive = self
                .directive_name_in_schema(subgraph_schema, &LIST_SIZE_DIRECTIVE_NAME_IN_SPEC)?
                .and_then(|name| source.get(name.as_str()));
            if let Some(list_size_directive) = list_size_directive {
                dest.push($wrap_ty(self.list_size_directive(
                    subgraph_schema,
                    list_size_directive.arguments.clone(),
                )?));
            }

            Ok(())
        }
    };
}

macro_rules! propagate_demand_control_directives_to_position {
    ($func_name:ident, $source_ty:ty, $dest_ty:ty) => {
        pub(crate) fn $func_name(
            &self,
            subgraph_schema: &mut FederationSchema,
            source: &Node<$source_ty>,
            dest: &$dest_ty,
        ) -> Result<(), FederationError> {
            let cost_directive = self
                .directive_name_in_schema(subgraph_schema, &COST_DIRECTIVE_NAME_IN_SPEC)?
                .and_then(|name| source.directives.get(name.as_str()));
            if let Some(cost_directive) = cost_directive {
                dest.insert_directive(
                    subgraph_schema,
                    Component::from(
                        self.cost_directive(subgraph_schema, cost_directive.arguments.clone())?,
                    ),
                )?;
            }

            let list_size_directive = self
                .directive_name_in_schema(subgraph_schema, &LIST_SIZE_DIRECTIVE_NAME_IN_SPEC)?
                .and_then(|name| source.directives.get(name.as_str()));
            if let Some(list_size_directive) = list_size_directive {
                dest.insert_directive(
                    subgraph_schema,
                    Component::from(self.list_size_directive(
                        subgraph_schema,
                        list_size_directive.arguments.clone(),
                    )?),
                )?;
            }

            Ok(())
        }
    };
}

impl CostSpecDefinition {
    pub(crate) fn new(version: Version, minimum_federation_version: Option<Version>) -> Self {
        Self {
            url: Url {
                identity: Identity::cost_identity(),
                version,
            },
            minimum_federation_version,
        }
    }

    pub(crate) fn cost_directive(
        &self,
        schema: &FederationSchema,
        arguments: Vec<Node<Argument>>,
    ) -> Result<Directive, FederationError> {
        let name = self
            .directive_name_in_schema(schema, &COST_DIRECTIVE_NAME_IN_SPEC)?
            .unwrap_or(COST_DIRECTIVE_NAME_DEFAULT);

        Ok(Directive { name, arguments })
    }

    pub(crate) fn list_size_directive(
        &self,
        schema: &FederationSchema,
        arguments: Vec<Node<Argument>>,
    ) -> Result<Directive, FederationError> {
        let name = self
            .directive_name_in_schema(schema, &LIST_SIZE_DIRECTIVE_NAME_IN_SPEC)?
            .unwrap_or(LIST_SIZE_DIRECTIVE_NAME_DEFAULT);

        Ok(Directive { name, arguments })
    }

    propagate_demand_control_directives!(
        propagate_demand_control_directives,
        apollo_compiler::ast::DirectiveList,
        Node::new
    );
    propagate_demand_control_directives_to_position!(
        propagate_demand_control_directives_for_enum,
        EnumType,
        EnumTypeDefinitionPosition
    );
    propagate_demand_control_directives_to_position!(
        propagate_demand_control_directives_for_object,
        ObjectType,
        ObjectTypeDefinitionPosition
    );
    propagate_demand_control_directives_to_position!(
        propagate_demand_control_directives_for_scalar,
        ScalarType,
        ScalarTypeDefinitionPosition
    );

    pub fn for_schema(
        schema: &apollo_compiler::Schema,
    ) -> Result<Option<&'static Self>, FederationError> {
        let schema = FederationSchema::new(schema.clone())?;
        let cost_link = schema
            .metadata()
            .as_ref()
            .and_then(|metadata| metadata.for_identity(&Identity::cost_identity()));
        tracing::debug!("Schema has cost link? {}", cost_link.is_some());
        let cost_spec = cost_link.and_then(|link| COST_VERSIONS.find(&link.url.version));
        Ok(cost_spec)
    }

    pub fn cost_directive_name_in_schema(
        &self,
        schema: &apollo_compiler::Schema,
    ) -> Result<Option<Name>, FederationError> {
        let schema = FederationSchema::new(schema.clone())?;
        self.directive_name_in_schema(&schema, &COST_DIRECTIVE_NAME_IN_SPEC)
    }

    pub fn listsize_directive_name_in_schema(
        &self,
        schema: &apollo_compiler::Schema,
    ) -> Result<Option<Name>, FederationError> {
        let schema = FederationSchema::new(schema.clone())?;
        self.directive_name_in_schema(&schema, &LIST_SIZE_DIRECTIVE_NAME_IN_SPEC)
    }
}

impl SpecDefinition for CostSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn minimum_federation_version(&self) -> Option<&Version> {
        self.minimum_federation_version.as_ref()
    }
}

lazy_static! {
    pub(crate) static ref COST_VERSIONS: SpecDefinitions<CostSpecDefinition> = {
        let mut definitions = SpecDefinitions::new(Identity::cost_identity());
        definitions.add(CostSpecDefinition::new(
            Version { major: 0, minor: 1 },
            Some(Version { major: 2, minor: 9 }),
        ));
        definitions
    };
}
