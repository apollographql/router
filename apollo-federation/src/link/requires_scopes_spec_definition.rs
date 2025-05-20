use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::ast::Type;
use apollo_compiler::name;
use apollo_compiler::schema::DirectiveLocation;

use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::argument_composition_strategies::ArgumentCompositionStrategy;
use crate::schema::type_and_directive_specification::ArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveSpecification;
use crate::schema::type_and_directive_specification::ScalarTypeSpecification;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;

pub(crate) const REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC: Name = name!("requiresScopes");
pub(crate) const REQUIRES_SCOPES_SCOPE_TYPE_NAME_IN_SPEC: Name = name!("Scope");
pub(crate) const REQUIRES_SCOPES_SCOPES_ARGUMENT_NAME: Name = name!("scopes");

#[derive(Clone)]
pub(crate) struct RequiresScopesSpecDefinition {
    url: Url,
    minimum_federation_version: Version,
}

impl RequiresScopesSpecDefinition {
    pub(crate) fn new(version: Version, minimum_federation_version: Version) -> Self {
        Self {
            url: Url {
                identity: Identity::requires_scopes_identity(),
                version,
            },
            minimum_federation_version,
        }
    }

    fn directive_specification(&self) -> Box<dyn TypeAndDirectiveSpecification> {
        Box::new(DirectiveSpecification::new(
            REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC,
            &[DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: REQUIRES_SCOPES_SCOPES_ARGUMENT_NAME,
                    get_type: |_schema, link| {
                        // The type is [[ [Scope!]! ]!]
                        let scope_type_name = link
                            .map(|l| {
                                l.type_name_in_schema(&REQUIRES_SCOPES_SCOPE_TYPE_NAME_IN_SPEC)
                            })
                            .unwrap_or(REQUIRES_SCOPES_SCOPE_TYPE_NAME_IN_SPEC);
                        Ok(Type::NonNullList(Box::new(Type::List(Box::new(
                            Type::NonNullList(Box::new(Type::List(Box::new(Type::NonNullNamed(
                                scope_type_name,
                            ))))),
                        )))))
                    },
                    default_value: None,
                },
                composition_strategy: Some(ArgumentCompositionStrategy::Union),
            }],
            false, // not repeatable
            &[
                DirectiveLocation::FieldDefinition,
                DirectiveLocation::Object,
                DirectiveLocation::Interface,
                DirectiveLocation::Scalar,
                DirectiveLocation::Enum,
            ],
            true, // composes
            Some(&|v| REQUIRES_SCOPES_VERSIONS.get_minimum_required_version(v)),
            None,
        ))
    }

    fn scalar_type_specification(&self) -> Box<dyn TypeAndDirectiveSpecification> {
        Box::new(ScalarTypeSpecification {
            name: REQUIRES_SCOPES_SCOPE_TYPE_NAME_IN_SPEC,
        })
    }
}

impl SpecDefinition for RequiresScopesSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn directive_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        vec![self.directive_specification()]
    }

    fn type_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        vec![self.scalar_type_specification()]
    }

    fn minimum_federation_version(&self) -> &Version {
        &self.minimum_federation_version
    }
}

pub(crate) static REQUIRES_SCOPES_VERSIONS: LazyLock<
    SpecDefinitions<RequiresScopesSpecDefinition>,
> = LazyLock::new(|| {
    let mut definitions = SpecDefinitions::new(Identity::requires_scopes_identity());
    definitions.add(RequiresScopesSpecDefinition::new(
        Version { major: 0, minor: 1 },
        Version { major: 2, minor: 5 },
    ));
    definitions
});
