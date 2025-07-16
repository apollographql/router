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
                        let scope_type_name = link
                            .map(|l| {
                                l.type_name_in_schema(&REQUIRES_SCOPES_SCOPE_TYPE_NAME_IN_SPEC)
                            })
                            .unwrap_or(REQUIRES_SCOPES_SCOPE_TYPE_NAME_IN_SPEC);
                        // The type is [[Scope!]!]!
                        Ok(Type::NonNullList(Box::new(Type::NonNullList(Box::new(
                            Type::NonNullNamed(scope_type_name),
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
            Some(&|v| REQUIRES_SCOPES_VERSIONS.get_dyn_minimum_required_version(v)),
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

    fn purpose(&self) -> Option<Purpose> {
        Some(Purpose::SECURITY)
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

    fn requires_scopes_spec_directives_snapshot(schema: &FederationSchema) -> String {
        schema
            .schema()
            .directive_definitions
            .iter()
            .filter_map(|(name, def)| {
                if name.as_str().starts_with("requiresScopes") {
                    Some(def.to_string())
                } else {
                    None
                }
            })
            .join("\n")
    }

    fn requires_scopes_spec_types_snapshot(schema: &FederationSchema) -> String {
        schema
            .schema()
            .types
            .iter()
            .filter_map(|(name, ty)| {
                if name.as_str().ends_with("__Scope") {
                    Some(ty.to_string())
                } else {
                    None
                }
            })
            .join("\n")
    }

    #[test]
    fn requires_scopes_spec_v0_1_definitions() {
        let schema = trivial_schema();
        let types_snapshot = requires_scopes_spec_types_snapshot(&schema);
        let expected_types = r#"scalar federation__Scope"#;
        assert_eq!(types_snapshot.trim(), expected_types.trim());

        let directives_snapshot: String = requires_scopes_spec_directives_snapshot(&schema);
        let expected_directives = r#"directive @requiresScopes(scopes: [[federation__Scope!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM"#;
        assert_eq!(directives_snapshot.trim(), expected_directives.trim());
    }
}
