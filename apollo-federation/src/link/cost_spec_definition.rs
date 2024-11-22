use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use lazy_static::lazy_static;
use std::collections::HashSet;

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

const COST_DIRECTIVE_NAME: Name = name!("cost");
const COST_DIRECTIVE_WEIGHT_ARGUMENT_NAME: Name = name!("weight");
const LIST_SIZE_DIRECTIVE_NAME: Name = name!("listSize");
const LIST_SIZE_DIRECTIVE_ASSUMED_SIZE_ARGUMENT_NAME: Name = name!("assumedSize");
const LIST_SIZE_DIRECTIVE_SLICING_ARGUMENTS_ARGUMENT_NAME: Name = name!("slicingArguments");
const LIST_SIZE_DIRECTIVE_SIZED_FIELDS_ARGUMENT_NAME: Name = name!("sizedFields");
const LIST_SIZE_DIRECTIVE_REQUIRE_ONE_SLICING_ARGUMENT_ARGUMENT_NAME: Name =
    name!("requireOneSlicingArgument");

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
        DirectiveList,
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

    fn for_federation_schema(
        schema: &FederationSchema,
    ) -> Result<Option<&'static Self>, FederationError> {
        let cost_link = schema
            .metadata()
            .as_ref()
            .and_then(|metadata| metadata.for_identity(&Identity::cost_identity()));
        let cost_spec = cost_link.and_then(|link| COST_VERSIONS.find(&link.url.version));
        Ok(cost_spec)
    }

    /// Returns the name of the `@cost` directive in the given schema, accounting for import aliases or specification name
    /// prefixes such as `@federation__cost`. This checks the linked cost specification, if there is one, and falls back
    /// to the federation spec.
    fn cost_directive_name(schema: &Schema) -> Result<Option<Name>, FederationError> {
        // TODO: Update FederationSchema to take Arc<Schema> so we don't have to clone
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

    /// Returns the name of the `@listSize` directive in the given schema, accounting for import aliases or specification name
    /// prefixes such as `@federation__listSize`. This checks the linked cost specification, if there is one, and falls back
    /// to the federation spec.
    fn list_size_directive_name(schema: &Schema) -> Result<Option<Name>, FederationError> {
        // TODO: Update FederationSchema to take Arc<Schema> so we don't have to clone
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

    pub fn cost_directive_from_argument(
        schema: &Schema,
        argument: &InputValueDefinition,
        ty: &ExtendedType,
    ) -> Option<CostDirective> {
        let directive_name = Self::cost_directive_name(schema).ok().flatten()?;
        CostDirective::from_directives(&directive_name, &argument.directives).or(
            CostDirective::from_schema_directives(&directive_name, ty.directives()),
        )
    }

    pub fn cost_directive_from_field(
        schema: &Schema,
        field: &FieldDefinition,
        ty: &ExtendedType,
    ) -> Option<CostDirective> {
        let directive_name = Self::cost_directive_name(schema).ok().flatten()?;
        CostDirective::from_directives(&directive_name, &field.directives).or(
            CostDirective::from_schema_directives(&directive_name, ty.directives()),
        )
    }

    pub fn list_size_directive_from_field_definition(
        schema: &Schema,
        field: &FieldDefinition,
    ) -> Option<ListSizeDirective> {
        let directive_name = Self::list_size_directive_name(schema).ok().flatten()?;
        ListSizeDirective::from_field_definition(&directive_name, field)
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

pub struct CostDirective {
    weight: i32,
}

impl CostDirective {
    pub fn weight(&self) -> f64 {
        self.weight as f64
    }

    fn from_directives(directive_name: &Name, directives: &DirectiveList) -> Option<Self> {
        directives
            .get(directive_name)
            .and_then(|cost| cost.specified_argument_by_name(&COST_DIRECTIVE_WEIGHT_ARGUMENT_NAME))
            .and_then(|weight| weight.to_i32())
            .map(|weight| Self { weight })
    }

    fn from_schema_directives(
        directive_name: &Name,
        directives: &apollo_compiler::schema::DirectiveList,
    ) -> Option<Self> {
        directives
            .get(directive_name)
            .and_then(|cost| cost.specified_argument_by_name(&COST_DIRECTIVE_WEIGHT_ARGUMENT_NAME))
            .and_then(|weight| weight.to_i32())
            .map(|weight| Self { weight })
    }
}

pub struct ListSizeDirective {
    pub assumed_size: Option<i32>,
    pub slicing_argument_names: Option<HashSet<String>>,
    pub sized_fields: Option<HashSet<String>>,
    pub require_one_slicing_argument: bool,
}

impl ListSizeDirective {
    pub fn from_field_definition(
        directive_name: &Name,
        definition: &FieldDefinition,
    ) -> Option<Self> {
        let directive = definition.directives.get(&directive_name);
        if let Some(directive) = directive {
            let assumed_size = directive
                .specified_argument_by_name(&LIST_SIZE_DIRECTIVE_ASSUMED_SIZE_ARGUMENT_NAME)
                .and_then(|arg| arg.to_i32());
            let slicing_argument_names = directive
                .specified_argument_by_name(&LIST_SIZE_DIRECTIVE_SLICING_ARGUMENTS_ARGUMENT_NAME)
                .and_then(|arg| arg.as_list())
                .map(|arg_list| {
                    arg_list
                        .iter()
                        .flat_map(|arg| arg.as_str())
                        .map(String::from)
                        .collect()
                });
            let sized_fields = directive
                .specified_argument_by_name(&LIST_SIZE_DIRECTIVE_SIZED_FIELDS_ARGUMENT_NAME)
                .and_then(|arg| arg.as_list())
                .map(|arg_list| {
                    arg_list
                        .iter()
                        .flat_map(|arg| arg.as_str())
                        .map(String::from)
                        .collect()
                });
            let require_one_slicing_argument = directive
                .specified_argument_by_name(
                    &LIST_SIZE_DIRECTIVE_REQUIRE_ONE_SLICING_ARGUMENT_ARGUMENT_NAME,
                )
                .and_then(|arg| arg.to_bool())
                .unwrap_or(true);

            Some(Self {
                assumed_size,
                slicing_argument_names,
                sized_fields,
                require_one_slicing_argument,
            })
        } else {
            None
        }
    }
}
