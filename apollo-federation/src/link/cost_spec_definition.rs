use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use lazy_static::lazy_static;

use crate::error::FederationError;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;
use crate::schema::FederationSchema;

pub const COST_DIRECTIVE_NAME: Name = name!("cost");
pub const LIST_SIZE_DIRECTIVE_NAME: Name = name!("listSize");

#[derive(Clone)]
pub struct CostSpecDefinition {
    url: Url,
    minimum_federation_version: Option<Version>,
}

macro_rules! propagate_demand_control_directives {
    ($func_name:ident, $directives_ty:ty, $wrap_ty:expr) => {
        pub(crate) fn $func_name(
            supergraph_schema: &FederationSchema,
            source: &$directives_ty,
            subgraph_schema: &FederationSchema,
            dest: &mut $directives_ty,
        ) -> Result<(), FederationError> {
            let cost_directive = Self::cost_directive_name(supergraph_schema.schema())?
                .and_then(|name| source.get(name.as_str()));
            if let Some(cost_directive) = cost_directive {
                dest.push($wrap_ty(Self::cost_directive(
                    subgraph_schema.schema(),
                    cost_directive.arguments.clone(),
                )?));
            }

            let list_size_directive = Self::list_size_directive_name(supergraph_schema.schema())?
                .and_then(|name| source.get(name.as_str()));
            if let Some(list_size_directive) = list_size_directive {
                dest.push($wrap_ty(Self::list_size_directive(
                    subgraph_schema.schema(),
                    list_size_directive.arguments.clone(),
                )?));
            }

            Ok(())
        }
    };
}

macro_rules! propagate_demand_control_directives_to_position {
    ($func_name:ident, $source_ty:ty, $pos_ty:ty) => {
        pub(crate) fn $func_name(
            supergraph_schema: &FederationSchema,
            subgraph_schema: &mut FederationSchema,
            pos: &$pos_ty,
        ) -> Result<(), FederationError> {
            let schema = supergraph_schema.schema();
            let source = pos.get(schema)?;
            let cost_directive = Self::cost_directive_name(schema)?
                .and_then(|name| source.directives.get(name.as_str()));
            if let Some(cost_directive) = cost_directive {
                pos.insert_directive(
                    subgraph_schema,
                    Component::from(Self::cost_directive(
                        subgraph_schema.schema(),
                        cost_directive.arguments.clone(),
                    )?),
                )?;
            }

            let list_size_directive = Self::list_size_directive_name(schema)?
                .and_then(|name| source.directives.get(name.as_str()));
            if let Some(list_size_directive) = list_size_directive {
                pos.insert_directive(
                    subgraph_schema,
                    Component::from(Self::list_size_directive(
                        subgraph_schema.schema(),
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
        schema: &Schema,
        arguments: Vec<Node<Argument>>,
    ) -> Result<Directive, FederationError> {
        // TODO: Handle no directive name
        let name = Self::cost_directive_name(schema)?.expect("has name");

        Ok(Directive { name, arguments })
    }

    pub(crate) fn list_size_directive(
        schema: &Schema,
        arguments: Vec<Node<Argument>>,
    ) -> Result<Directive, FederationError> {
        // TODO: Handle no directive name
        let name = Self::list_size_directive_name(schema)?.expect("has name");

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

    pub(crate) fn for_federation_schema(
        schema: &FederationSchema,
    ) -> Result<Option<&'static Self>, FederationError> {
        let cost_link = schema
            .metadata()
            .as_ref()
            .and_then(|metadata| metadata.for_identity(&Identity::cost_identity()));
        let cost_spec = cost_link.and_then(|link| COST_VERSIONS.find(&link.url.version));
        Ok(cost_spec)
    }

    /// Returns the name of the `@cost` directive in the given schema. This attempts to find the imported cost
    /// specification and to use the imported or default name from the imported link, as is available. If the
    /// cost specification is not directly imported, this will return the cost directive name from the
    /// federation specification.
    pub fn cost_directive_name(schema: &Schema) -> Result<Option<Name>, FederationError> {
        let schema = FederationSchema::new(schema.clone())?;
        if let Some(name) = Self::for_federation_schema(&schema)?.and_then(|spec| {
            spec.directive_name_in_schema(&schema, &COST_DIRECTIVE_NAME)
                .ok()
                .flatten()
        }) {
            Ok(Some(name))
        } else if let Ok(fed_spec) = get_federation_spec_definition_from_subgraph(&schema) {
            fed_spec.directive_name_in_schema(&schema, &COST_DIRECTIVE_NAME)
        } else {
            Ok(None)
        }
    }

    /// Returns the name of the `@listSize` directive in the given schema. This attempts to find the imported cost
    /// specification and to use the imported or default name from the imported link, as is available. If the
    /// cost specification is not directly imported, this will return the listSize directive name from the
    /// federation specification.
    pub fn list_size_directive_name(schema: &Schema) -> Result<Option<Name>, FederationError> {
        let schema = FederationSchema::new(schema.clone())?;
        if let Some(name) = Self::for_federation_schema(&schema)?.and_then(|spec| {
            spec.directive_name_in_schema(&schema, &LIST_SIZE_DIRECTIVE_NAME)
                .ok()
                .flatten()
        }) {
            Ok(Some(name))
        } else if let Ok(fed_spec) = get_federation_spec_definition_from_subgraph(&schema) {
            fed_spec.directive_name_in_schema(&schema, &LIST_SIZE_DIRECTIVE_NAME)
        } else {
            Ok(None)
        }
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
