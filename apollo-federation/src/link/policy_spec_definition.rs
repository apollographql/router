use std::sync::Arc;
use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::ast::Type;
use apollo_compiler::name;
use apollo_compiler::schema::DirectiveLocation;

use crate::link::Purpose;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitionLookup;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::argument_composition_strategies::ArgumentCompositionStrategy;
use crate::schema::type_and_directive_specification::ArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveSpecification;
use crate::schema::type_and_directive_specification::ScalarTypeSpecification;

pub(crate) const POLICY_DIRECTIVE_NAME_IN_SPEC: Name = name!("policy");
pub(crate) const POLICY_POLICY_TYPE_NAME_IN_SPEC: Name = name!("Policy");
pub(crate) const POLICY_POLICIES_ARGUMENT_NAME: Name = name!("policies");

pub(crate) struct PolicySpecDefinition {
    url: Url,
    minimum_federation_version: Version,
    specs: SpecDefinitionLookup,
}

impl PolicySpecDefinition {
    pub(crate) fn new(version: Version, minimum_federation_version: Version) -> Self {
        let policy_directive_spec = Self::directive_specification();
        let policy_scalar_spec = Self::scalar_type_specification();

        Self {
            url: Url {
                identity: Identity::policy_identity(),
                version,
            },
            minimum_federation_version,
            specs: SpecDefinitionLookup::from([
                (
                    policy_directive_spec.name().clone(),
                    Arc::new(policy_directive_spec.into()),
                ),
                (
                    policy_scalar_spec.name().clone(),
                    Arc::new(policy_scalar_spec.into()),
                ),
            ]),
        }
    }

    fn directive_specification() -> DirectiveSpecification {
        DirectiveSpecification::new(
            POLICY_DIRECTIVE_NAME_IN_SPEC,
            &[DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: POLICY_POLICIES_ARGUMENT_NAME,
                    get_type: |_schema, link| {
                        let policy_type_name = link
                            .map(|l| l.type_name_in_schema(&POLICY_POLICY_TYPE_NAME_IN_SPEC))
                            .unwrap_or(POLICY_POLICY_TYPE_NAME_IN_SPEC);
                        // The type is [[Policy!]!]!
                        Ok(Type::NonNullList(Box::new(Type::NonNullList(Box::new(
                            Type::NonNullNamed(policy_type_name),
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
            Some(&|v| POLICY_VERSIONS.get_dyn_minimum_required_version(v)),
            None,
        )
    }

    fn scalar_type_specification() -> ScalarTypeSpecification {
        ScalarTypeSpecification {
            name: POLICY_POLICY_TYPE_NAME_IN_SPEC,
        }
    }
}

impl SpecDefinition for PolicySpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn minimum_federation_version(&self) -> &Version {
        &self.minimum_federation_version
    }

    fn purpose(&self) -> Option<Purpose> {
        Some(Purpose::SECURITY)
    }

    fn specs(&self) -> &SpecDefinitionLookup {
        &self.specs
    }
}

pub(crate) static POLICY_VERSIONS: LazyLock<SpecDefinitions<PolicySpecDefinition>> =
    LazyLock::new(|| {
        let mut definitions = SpecDefinitions::new(Identity::policy_identity());
        definitions.add(PolicySpecDefinition::new(
            Version { major: 0, minor: 1 },
            Version { major: 2, minor: 6 },
        ));
        definitions
    });

#[cfg(test)]
mod test {
    use itertools::Itertools;

    use crate::schema::FederationSchema;
    use crate::subgraph::test_utils::BuildOption;
    use crate::subgraph::test_utils::build_inner_expanded;

    fn trivial_schema() -> FederationSchema {
        build_inner_expanded("type Query { hello: String }", BuildOption::AsFed2)
            .unwrap()
            .schema()
            .to_owned()
    }

    fn policy_spec_directives_snapshot(schema: &FederationSchema) -> String {
        schema
            .schema()
            .directive_definitions
            .iter()
            .filter_map(|(name, def)| {
                if name.as_str().starts_with("policy") {
                    Some(def.to_string())
                } else {
                    None
                }
            })
            .join("\n")
    }

    fn policy_spec_types_snapshot(schema: &FederationSchema) -> String {
        schema
            .schema()
            .types
            .iter()
            .filter_map(|(name, ty)| {
                if name.as_str().ends_with("__Policy") {
                    Some(ty.to_string())
                } else {
                    None
                }
            })
            .join("")
    }

    #[test]
    fn policy_spec_v0_1_definitions() {
        let schema = trivial_schema();
        let types_snapshot = policy_spec_types_snapshot(&schema);
        let expected_types = r#"scalar federation__Policy"#;
        assert_eq!(types_snapshot.trim(), expected_types.trim());

        let directives_snapshot = policy_spec_directives_snapshot(&schema);
        let expected_directives = r#"directive @policy(policies: [[federation__Policy!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM"#;
        assert_eq!(directives_snapshot.trim(), expected_directives.trim());
    }
}
