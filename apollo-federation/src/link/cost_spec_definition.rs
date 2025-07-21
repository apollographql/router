use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::Value;
use apollo_compiler::ty;
use indexmap::IndexSet;

use crate::error::FederationError;
use crate::internal_error;
use crate::link::Purpose;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::FederationSchema;
use crate::schema::argument_composition_strategies::ArgumentCompositionStrategy;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;
use crate::schema::type_and_directive_specification::ArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveSpecification;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;

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
    minimum_federation_version: Version,
}

macro_rules! propagate_demand_control_directives {
    ($func_name:ident, $directives_ty:ty, $wrap_ty:expr) => {
        pub(crate) fn $func_name(
            supergraph_schema: &FederationSchema,
            source: &$directives_ty,
            subgraph_schema: &FederationSchema,
            dest: &mut $directives_ty,
        ) -> Result<(), FederationError> {
            let cost_directive = Self::cost_directive_name(supergraph_schema)?
                .and_then(|name| source.get(name.as_str()));
            if let Some(cost_directive) = cost_directive {
                dest.push($wrap_ty(Self::cost_directive(
                    subgraph_schema,
                    cost_directive.arguments.clone(),
                )?));
            }

            let list_size_directive = Self::list_size_directive_name(supergraph_schema)?
                .and_then(|name| source.get(name.as_str()));
            if let Some(list_size_directive) = list_size_directive {
                dest.push($wrap_ty(Self::list_size_directive(
                    subgraph_schema,
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
            let source = pos.get(supergraph_schema.schema())?;
            let cost_directive = Self::cost_directive_name(supergraph_schema)?
                .and_then(|name| source.directives.get(name.as_str()));
            if let Some(cost_directive) = cost_directive {
                pos.insert_directive(
                    subgraph_schema,
                    Component::from(Self::cost_directive(
                        subgraph_schema,
                        cost_directive.arguments.clone(),
                    )?),
                )?;
            }

            let list_size_directive = Self::list_size_directive_name(supergraph_schema)?
                .and_then(|name| source.directives.get(name.as_str()));
            if let Some(list_size_directive) = list_size_directive {
                pos.insert_directive(
                    subgraph_schema,
                    Component::from(Self::list_size_directive(
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
    pub(crate) fn new(version: Version, minimum_federation_version: Version) -> Self {
        Self {
            url: Url {
                identity: Identity::cost_identity(),
                version,
            },
            minimum_federation_version,
        }
    }

    pub(crate) fn cost_directive(
        schema: &FederationSchema,
        arguments: Vec<Node<Argument>>,
    ) -> Result<Directive, FederationError> {
        let name = Self::cost_directive_name(schema)?.ok_or_else(|| {
            internal_error!("The \"@cost\" directive is undefined in the target schema")
        })?;

        Ok(Directive { name, arguments })
    }

    pub(crate) fn list_size_directive(
        schema: &FederationSchema,
        arguments: Vec<Node<Argument>>,
    ) -> Result<Directive, FederationError> {
        let name = Self::list_size_directive_name(schema)?.ok_or_else(|| {
            internal_error!("The \"@listSize\" directive is undefined in the target schema")
        })?;

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

    fn for_federation_schema(schema: &FederationSchema) -> Option<&'static Self> {
        let link = schema
            .metadata()?
            .for_identity(&Identity::cost_identity())?;
        COST_VERSIONS.find(&link.url.version)
    }

    /// Returns the name of the `@cost` directive in the given schema, accounting for import aliases or specification name
    /// prefixes such as `@federation__cost`. This checks the linked cost specification, if there is one, and falls back
    /// to the federation spec.
    pub(crate) fn cost_directive_name(
        schema: &FederationSchema,
    ) -> Result<Option<Name>, FederationError> {
        if let Some(spec) = Self::for_federation_schema(schema) {
            spec.directive_name_in_schema(schema, &COST_DIRECTIVE_NAME)
        } else if let Ok(fed_spec) = get_federation_spec_definition_from_subgraph(schema) {
            fed_spec.directive_name_in_schema(schema, &COST_DIRECTIVE_NAME)
        } else {
            Ok(None)
        }
    }

    /// Returns the name of the `@listSize` directive in the given schema, accounting for import aliases or specification name
    /// prefixes such as `@federation__listSize`. This checks the linked cost specification, if there is one, and falls back
    /// to the federation spec.
    pub(crate) fn list_size_directive_name(
        schema: &FederationSchema,
    ) -> Result<Option<Name>, FederationError> {
        if let Some(spec) = Self::for_federation_schema(schema) {
            spec.directive_name_in_schema(schema, &LIST_SIZE_DIRECTIVE_NAME)
        } else if let Ok(fed_spec) = get_federation_spec_definition_from_subgraph(schema) {
            fed_spec.directive_name_in_schema(schema, &LIST_SIZE_DIRECTIVE_NAME)
        } else {
            Ok(None)
        }
    }

    pub fn cost_directive_from_argument(
        schema: &FederationSchema,
        argument: &InputValueDefinition,
        ty: &ExtendedType,
    ) -> Result<Option<CostDirective>, FederationError> {
        let directive_name = Self::cost_directive_name(schema)?;
        if let Some(name) = directive_name.as_ref() {
            Ok(CostDirective::from_directives(name, &argument.directives)
                .or(CostDirective::from_schema_directives(name, ty.directives())))
        } else {
            Ok(None)
        }
    }

    pub fn cost_directive_from_field(
        schema: &FederationSchema,
        field: &FieldDefinition,
        ty: &ExtendedType,
    ) -> Result<Option<CostDirective>, FederationError> {
        let directive_name = Self::cost_directive_name(schema)?;
        if let Some(name) = directive_name.as_ref() {
            Ok(CostDirective::from_directives(name, &field.directives)
                .or(CostDirective::from_schema_directives(name, ty.directives())))
        } else {
            Ok(None)
        }
    }

    pub fn list_size_directive_from_field_definition(
        schema: &FederationSchema,
        field: &FieldDefinition,
    ) -> Result<Option<ListSizeDirective>, FederationError> {
        let directive_name = Self::list_size_directive_name(schema)?;
        if let Some(name) = directive_name.as_ref() {
            Ok(ListSizeDirective::from_field_definition(name, field))
        } else {
            Ok(None)
        }
    }

    fn cost_directive_specification() -> DirectiveSpecification {
        DirectiveSpecification::new(
            COST_DIRECTIVE_NAME,
            &[DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: COST_DIRECTIVE_WEIGHT_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(Int!)),
                    default_value: None,
                },
                composition_strategy: Some(ArgumentCompositionStrategy::Max),
            }],
            false,
            &[
                DirectiveLocation::ArgumentDefinition,
                DirectiveLocation::Enum,
                DirectiveLocation::FieldDefinition,
                DirectiveLocation::InputFieldDefinition,
                DirectiveLocation::Object,
                DirectiveLocation::Scalar,
            ],
            true,
            Some(&|v| COST_VERSIONS.get_dyn_minimum_required_version(v)),
            None,
        )
    }

    fn list_size_directive_specification() -> DirectiveSpecification {
        DirectiveSpecification::new(
            LIST_SIZE_DIRECTIVE_NAME,
            &[
                DirectiveArgumentSpecification {
                    base_spec: ArgumentSpecification {
                        name: LIST_SIZE_DIRECTIVE_ASSUMED_SIZE_ARGUMENT_NAME,
                        get_type: |_, _| Ok(ty!(Int)),
                        default_value: None,
                    },
                    composition_strategy: Some(ArgumentCompositionStrategy::NullableMax),
                },
                DirectiveArgumentSpecification {
                    base_spec: ArgumentSpecification {
                        name: LIST_SIZE_DIRECTIVE_SLICING_ARGUMENTS_ARGUMENT_NAME,
                        get_type: |_, _| Ok(ty!([String!])),
                        default_value: None,
                    },
                    composition_strategy: Some(ArgumentCompositionStrategy::NullableUnion),
                },
                DirectiveArgumentSpecification {
                    base_spec: ArgumentSpecification {
                        name: LIST_SIZE_DIRECTIVE_SIZED_FIELDS_ARGUMENT_NAME,
                        get_type: |_, _| Ok(ty!([String!])),
                        default_value: None,
                    },
                    composition_strategy: Some(ArgumentCompositionStrategy::NullableUnion),
                },
                DirectiveArgumentSpecification {
                    base_spec: ArgumentSpecification {
                        name: LIST_SIZE_DIRECTIVE_REQUIRE_ONE_SLICING_ARGUMENT_ARGUMENT_NAME,
                        get_type: |_, _| Ok(ty!(Boolean)),
                        default_value: Some(Value::Boolean(true)),
                    },
                    composition_strategy: Some(ArgumentCompositionStrategy::NullableAnd),
                },
            ],
            false,
            &[DirectiveLocation::FieldDefinition],
            true,
            Some(&|v| COST_VERSIONS.get_dyn_minimum_required_version(v)),
            None,
        )
    }
}

impl SpecDefinition for CostSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn directive_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        vec![
            Box::new(Self::cost_directive_specification()),
            Box::new(Self::list_size_directive_specification()),
        ]
    }

    fn type_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        vec![]
    }

    fn minimum_federation_version(&self) -> &Version {
        &self.minimum_federation_version
    }

    fn purpose(&self) -> Option<Purpose> {
        None
    }
}

pub(crate) static COST_VERSIONS: LazyLock<SpecDefinitions<CostSpecDefinition>> =
    LazyLock::new(|| {
        let mut definitions = SpecDefinitions::new(Identity::cost_identity());
        definitions.add(CostSpecDefinition::new(
            Version { major: 0, minor: 1 },
            Version { major: 2, minor: 9 },
        ));
        definitions
    });

pub struct CostDirective {
    weight: i32,
}

impl CostDirective {
    pub fn weight(&self) -> f64 {
        self.weight as f64
    }

    pub(crate) fn from_directives(
        directive_name: &Name,
        directives: &DirectiveList,
    ) -> Option<Self> {
        directives
            .get(directive_name)?
            .specified_argument_by_name(&COST_DIRECTIVE_WEIGHT_ARGUMENT_NAME)?
            .to_i32()
            .map(|weight| Self { weight })
    }

    pub(crate) fn from_schema_directives(
        directive_name: &Name,
        directives: &apollo_compiler::schema::DirectiveList,
    ) -> Option<Self> {
        directives
            .get(directive_name)?
            .specified_argument_by_name(&COST_DIRECTIVE_WEIGHT_ARGUMENT_NAME)?
            .to_i32()
            .map(|weight| Self { weight })
    }
}

pub struct ListSizeDirective {
    pub assumed_size: Option<i32>,
    pub slicing_argument_names: Option<IndexSet<String>>,
    pub sized_fields: Option<IndexSet<String>>,
    pub require_one_slicing_argument: bool,
}

impl ListSizeDirective {
    pub fn from_field_definition(
        directive_name: &Name,
        definition: &FieldDefinition,
    ) -> Option<Self> {
        let directive = definition.directives.get(directive_name)?;
        let assumed_size = Self::assumed_size(directive);
        let slicing_argument_names = Self::slicing_argument_names(directive);
        let sized_fields = Self::sized_fields(directive);
        let require_one_slicing_argument =
            Self::require_one_slicing_argument(directive).unwrap_or(true);

        Some(Self {
            assumed_size,
            slicing_argument_names,
            sized_fields,
            require_one_slicing_argument,
        })
    }

    fn assumed_size(directive: &Directive) -> Option<i32> {
        directive
            .specified_argument_by_name(&LIST_SIZE_DIRECTIVE_ASSUMED_SIZE_ARGUMENT_NAME)?
            .to_i32()
    }

    fn slicing_argument_names(directive: &Directive) -> Option<IndexSet<String>> {
        let names = directive
            .specified_argument_by_name(&LIST_SIZE_DIRECTIVE_SLICING_ARGUMENTS_ARGUMENT_NAME)?
            .as_list()?
            .iter()
            .flat_map(|arg| arg.as_str())
            .map(String::from)
            .collect();
        Some(names)
    }

    fn sized_fields(directive: &Directive) -> Option<IndexSet<String>> {
        let fields = directive
            .specified_argument_by_name(&LIST_SIZE_DIRECTIVE_SIZED_FIELDS_ARGUMENT_NAME)?
            .as_list()?
            .iter()
            .flat_map(|arg| arg.as_str())
            .map(String::from)
            .collect();
        Some(fields)
    }

    fn require_one_slicing_argument(directive: &Directive) -> Option<bool> {
        directive
            .specified_argument_by_name(
                &LIST_SIZE_DIRECTIVE_REQUIRE_ONE_SLICING_ARGUMENT_ARGUMENT_NAME,
            )?
            .to_bool()
    }
}
