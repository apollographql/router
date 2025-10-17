use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::name;
use apollo_compiler::schema::DirectiveLocation;

use crate::link::Purpose;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::type_and_directive_specification::DirectiveSpecification;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;

pub(crate) const AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC: Name = name!("authenticated");

#[derive(Clone)]
pub(crate) struct AuthenticatedSpecDefinition {
    url: Url,
    minimum_federation_version: Version,
}

impl AuthenticatedSpecDefinition {
    pub(crate) fn new(version: Version, minimum_federation_version: Version) -> Self {
        Self {
            url: Url {
                identity: Identity::authenticated_identity(),
                version,
            },
            minimum_federation_version,
        }
    }

    fn directive_specification(&self) -> Box<dyn TypeAndDirectiveSpecification> {
        Box::new(DirectiveSpecification::new(
            AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC,
            &[],
            false, // not repeatable
            &[
                DirectiveLocation::FieldDefinition,
                DirectiveLocation::Object,
                DirectiveLocation::Interface,
                DirectiveLocation::Scalar,
                DirectiveLocation::Enum,
            ],
            true, // composes
            Some(&|v| AUTHENTICATED_VERSIONS.get_dyn_minimum_required_version(v)),
            None,
        ))
    }
}

impl SpecDefinition for AuthenticatedSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn directive_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        vec![self.directive_specification()]
    }

    fn type_specs(&self) -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
        vec![]
    }

    fn minimum_federation_version(&self) -> &Version {
        &self.minimum_federation_version
    }

    fn purpose(&self) -> Option<Purpose> {
        Some(Purpose::SECURITY)
    }
}

pub(crate) static AUTHENTICATED_VERSIONS: LazyLock<SpecDefinitions<AuthenticatedSpecDefinition>> =
    LazyLock::new(|| {
        let mut definitions = SpecDefinitions::new(Identity::authenticated_identity());
        definitions.add(AuthenticatedSpecDefinition::new(
            Version { major: 0, minor: 1 },
            Version { major: 2, minor: 5 },
        ));
        definitions
    });

#[cfg(test)]
mod test {
    use itertools::Itertools;

    use crate::subgraph::test_utils::BuildOption;
    use crate::subgraph::test_utils::build_inner_expanded;

    fn trivial_schema() -> crate::schema::FederationSchema {
        build_inner_expanded("type Query { hello: String }", BuildOption::AsFed2)
            .unwrap()
            .schema()
            .to_owned()
    }

    fn authenticated_spec_directives_snapshot(schema: &crate::schema::FederationSchema) -> String {
        schema
            .schema()
            .directive_definitions
            .iter()
            .filter_map(|(name, def)| {
                if name.as_str().starts_with("authenticated") {
                    Some(def.to_string())
                } else {
                    None
                }
            })
            .join("\n")
    }

    #[test]
    fn authenticated_spec_v0_1_definitions() {
        let schema = trivial_schema();
        let snapshot = authenticated_spec_directives_snapshot(&schema);
        let expected =
            r#"directive @authenticated on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM"#;
        assert_eq!(snapshot.trim(), expected.trim());
    }
}
