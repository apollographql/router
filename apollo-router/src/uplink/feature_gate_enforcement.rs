use std::collections::HashMap;
use std::fmt::Display;
use std::fmt::Formatter;

use itertools::Itertools;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use super::parsed_link_spec::ParsedLinkSpec;
use crate::Configuration;
use crate::spec::LINK_DIRECTIVE_NAME;
use crate::spec::Schema;

#[derive(Debug)]
pub(crate) struct FeatureGateEnforcementReport {
    gated_features_in_use: Vec<FeatureGateViolation>,
}

impl FeatureGateEnforcementReport {
    pub(crate) fn check(&self) -> Result<(), Vec<FeatureGateViolation>> {
        if self.gated_features_in_use.is_empty() {
            Ok(())
        } else {
            Err(self.gated_features_in_use.clone())
        }
    }

    pub(crate) fn build(
        configuration: &Configuration,
        schema: &Schema,
    ) -> FeatureGateEnforcementReport {
        FeatureGateEnforcementReport {
            gated_features_in_use: Self::validate_schema(
                schema,
                &Self::schema_restrictions(),
                configuration,
            ),
        }
    }

    fn validate_schema(
        schema: &Schema,
        schema_restrictions: &Vec<FeatureRestriction>,
        configuration: &Configuration,
    ) -> Vec<FeatureGateViolation> {
        let link_specs_in_join_directive = schema
            .supergraph_schema()
            .schema_definition
            .directives
            .get_all("join__directive")
            .filter(|join| {
                join.specified_argument_by_name("name")
                    .and_then(|name| name.as_str())
                    .map(|name| name == LINK_DIRECTIVE_NAME)
                    .unwrap_or_default()
            })
            .filter_map(|join| {
                join.specified_argument_by_name("args")
                    .and_then(|arg| arg.as_object())
            })
            .filter_map(|link| {
                ParsedLinkSpec::from_join_directive_args(link).map(|maybe_spec| {
                    maybe_spec.ok().map(|spec| (spec.spec_url.to_owned(), spec))
                })?
            })
            .collect::<HashMap<_, _>>();

        let mut schema_violations: Vec<FeatureGateViolation> = Vec::new();

        for restriction in schema_restrictions {
            let mut selector = jsonpath_lib::selector(
                configuration
                    .validated_yaml
                    .as_ref()
                    .unwrap_or(&Value::Null),
            );

            match restriction {
                FeatureRestriction::SpecInJoinDirective {
                    spec_url,
                    name,
                    version_req,
                    feature_gate_configuration_path,
                    expected_value,
                    to_enable,
                } => {
                    if let Some(link_spec) = link_specs_in_join_directive.get(spec_url) {
                        if version_req.matches(&link_spec.version) {
                            if let Some(config_value) = selector(feature_gate_configuration_path)
                                .expect("path on restriction was not valid")
                                .first()
                            {
                                if *config_value != expected_value {
                                    schema_violations.push(FeatureGateViolation::Spec {
                                        url: link_spec.url.to_string(),
                                        name: name.to_string(),
                                        to_enable: to_enable.to_string(),
                                    });
                                }
                            } else {
                                schema_violations.push(FeatureGateViolation::Spec {
                                    url: link_spec.url.to_string(),
                                    name: name.to_string(),
                                    to_enable: to_enable.to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }

        schema_violations
    }

    fn schema_restrictions() -> Vec<FeatureRestriction> {
        // @link(url: "https://specs.apollo.dev/connect/v0.2") requires `connectors.preview_connect_v0_2: true`
        // This uses join__directives to find specs because the we're looking
        // at links within individual subgraphs.
        vec![FeatureRestriction::SpecInJoinDirective {
            name: "Connect v0.2".to_string(),
            spec_url: "https://specs.apollo.dev/connect".to_string(),
            version_req: semver::VersionReq {
                comparators: vec![semver::Comparator {
                    op: semver::Op::Exact,
                    major: 0,
                    minor: 2.into(),
                    patch: 0.into(),
                    pre: semver::Prerelease::EMPTY,
                }],
            },
            feature_gate_configuration_path: "$.connectors.preview_connect_v0_2".to_string(),
            expected_value: Value::Bool(true),
            to_enable: "  connectors:
    preview_connect_v0_2: true"
                .to_string(),
        }]
    }
}

impl Display for FeatureGateEnforcementReport {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if !self.gated_features_in_use.is_empty() {
            let restricted_schema = self
                .gated_features_in_use
                .iter()
                .map(|v| v.to_string())
                .join("\n\n");

            write!(f, "Schema features:\n{restricted_schema}")?
        }

        Ok(())
    }
}

/// An individual check for the supergraph schema
#[derive(Clone, Debug)]
pub(crate) enum FeatureRestriction {
    SpecInJoinDirective {
        spec_url: String,
        name: String,
        version_req: semver::VersionReq,
        feature_gate_configuration_path: String,
        expected_value: Value,
        to_enable: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum FeatureGateViolation {
    Spec {
        url: String,
        name: String,
        to_enable: String,
    },
}

impl Display for FeatureGateViolation {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            FeatureGateViolation::Spec {
                name,
                url,
                to_enable,
            } => {
                write!(
                    f,
                    "* {} @link(url: \"{}\")\n  To enable:\n\n{}",
                    name, url, to_enable
                )
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;

    use super::FeatureGateEnforcementReport;
    use super::FeatureGateViolation;
    use crate::Configuration;
    use crate::spec::Schema;

    fn check(router_yaml: &str, supergraph_schema: &str) -> FeatureGateEnforcementReport {
        let config = Configuration::from_str(router_yaml).expect("router config must be valid");
        let schema =
            Schema::parse(supergraph_schema, &config).expect("supergraph schema must be valid");
        FeatureGateEnforcementReport::build(&config, &schema)
    }

    #[test]
    fn feature_gate_connectors_v0_2() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/feature_enforcement_connect_v0_2.graphql"),
        );

        assert_eq!(
            1,
            report.gated_features_in_use.len(),
            "should have found restricted connect feature"
        );
        let FeatureGateViolation::Spec { url, name, .. } = &report.gated_features_in_use[0];

        assert_eq!("https://specs.apollo.dev/connect/v0.2", url);
        assert_eq!("Connect v0.2", name);
    }

    #[test]
    fn feature_gate_connectors_v0_2_enabled() {
        let report = check(
            include_str!("testdata/connectv0_2.router.yaml"),
            include_str!("testdata/feature_enforcement_connect_v0_2.graphql"),
        );

        assert_eq!(
            0,
            report.gated_features_in_use.len(),
            "should have found restricted connect feature"
        );
    }

    #[test]
    fn feature_gate_connectors_v0_1_noop() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/feature_enforcement_connect_v0_1.graphql"),
        );

        assert_eq!(
            0,
            report.gated_features_in_use.len(),
            "should have found restricted connect feature"
        );
    }
}
