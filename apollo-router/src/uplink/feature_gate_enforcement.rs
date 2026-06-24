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
            .get_all(LINK_DIRECTIVE_NAME)
            .filter_map(|link| {
                ParsedLinkSpec::from_link_directive(link).map(|maybe_spec| {
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
                    warning,
                } => {
                    if let Some(link_spec) = link_specs_in_join_directive.get(spec_url) {
                        let relevant = version_req.matches(&link_spec.version);
                        let enabled = selector(feature_gate_configuration_path)
                            .expect("path on restriction was not valid")
                            .first()
                            .is_some_and(|config_value| *config_value == expected_value);

                        if relevant && enabled && warning.is_some() {
                            tracing::warn!("{}", warning.as_ref().unwrap_or(&"".to_string()));
                        }

                        if relevant && !enabled {
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

        schema_violations
    }

    fn schema_restrictions() -> Vec<FeatureRestriction> {
        // No connect spec version is currently feature-gated.
        //
        // connect/v0.4 used to require `connectors.preview_connect_v0_4: true` in
        // router.yaml, but that opt-in was removed: `@link`-ing connect/v0.4 in a
        // subgraph is itself a sufficient opt-in, and the extra config hoop was
        // just friction we'd want to drop eventually anyway.
        //
        // The enforcement machinery below is intentionally retained. To gate a
        // future preview spec (e.g. connect/v0.5), push a `FeatureRestriction`
        // entry here with `feature_gate_configuration_path` pointing at that
        // version's own config key (e.g. `$.connectors.preview_connect_v0_5`).
        vec![]
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

/// An individual check for the supergraph schema.
///
/// No variant is constructed while `schema_restrictions` is empty, but the
/// machinery is retained for gating future preview specs — see the comment in
/// [`FeatureGateEnforcementReport::schema_restrictions`].
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) enum FeatureRestriction {
    SpecInJoinDirective {
        spec_url: String,
        name: String,
        version_req: semver::VersionReq,
        feature_gate_configuration_path: String,
        expected_value: Value,
        to_enable: String,
        warning: Option<String>,
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
                    "* {name} @link(url: \"{url}\")\n  To enable:\n\n{to_enable}"
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
    use super::FeatureRestriction;
    use crate::Configuration;
    use crate::spec::Schema;

    fn check(router_yaml: &str, supergraph_schema: &str) -> FeatureGateEnforcementReport {
        let config = Configuration::from_str(router_yaml).expect("router config must be valid");
        let schema =
            Schema::parse(supergraph_schema, &config).expect("supergraph schema must be valid");
        FeatureGateEnforcementReport::build(&config, &schema)
    }

    #[test]
    fn connect_v0_4_no_longer_gated() {
        // connect/v0.4 used to require `connectors.preview_connect_v0_4: true`.
        // With the opt-in removed it must load with no flag and no violation.
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/feature_enforcement_connect_v0_4.graphql"),
        );

        assert_eq!(
            0,
            report.gated_features_in_use.len(),
            "connect/v0.4 should no longer be feature-gated"
        );
    }

    #[test]
    fn preview_connect_v0_4_flag_is_a_noop() {
        // The flag is now a deprecated no-op, but configs that still set it must
        // continue to parse and load without a violation.
        let report = check(
            include_str!("testdata/connectv0_4.router.yaml"),
            include_str!("testdata/feature_enforcement_connect_v0_4.graphql"),
        );

        assert_eq!(
            0,
            report.gated_features_in_use.len(),
            "setting the deprecated flag should not produce a violation"
        );
    }

    #[test]
    fn feature_gate_connectors_v0_2_noop() {
        let report = check(
            include_str!("testdata/oss.router.yaml"),
            include_str!("testdata/feature_enforcement_connect_v0_2.graphql"),
        );

        assert_eq!(
            0,
            report.gated_features_in_use.len(),
            "should not have found restricted connect feature"
        );
    }

    fn connect_v0_4_restriction() -> FeatureRestriction {
        FeatureRestriction::SpecInJoinDirective {
            name: "Connect v0.4".to_string(),
            spec_url: "https://specs.apollo.dev/connect".to_string(),
            version_req: semver::VersionReq {
                comparators: vec![semver::Comparator {
                    op: semver::Op::Exact,
                    major: 0,
                    minor: 4.into(),
                    patch: 0.into(),
                    pre: semver::Prerelease::EMPTY,
                }],
            },
            feature_gate_configuration_path: "$.connectors.preview_connect_v0_4".to_string(),
            expected_value: serde_json::Value::Bool(true),
            to_enable: "  connectors:\n    preview_connect_v0_4: true".to_string(),
            warning: None,
        }
    }

    fn violations(
        router_yaml: &str,
        restrictions: &[FeatureRestriction],
    ) -> Vec<FeatureGateViolation> {
        let config = Configuration::from_str(router_yaml).expect("router config must be valid");
        let schema = Schema::parse(
            include_str!("testdata/feature_enforcement_connect_v0_4.graphql"),
            &config,
        )
        .expect("supergraph schema must be valid");
        FeatureGateEnforcementReport::validate_schema(&schema, &restrictions.to_vec(), &config)
    }

    /// `schema_restrictions` is currently empty, so exercise the enforcement
    /// machinery directly to prove it still gates a spec when configured — the
    /// shape a future connect/v0.5 gate would take.
    #[test]
    fn machinery_still_gates_when_a_restriction_is_present() {
        let restrictions = [connect_v0_4_restriction()];

        let without_flag = violations(include_str!("testdata/oss.router.yaml"), &restrictions);
        assert_eq!(
            1,
            without_flag.len(),
            "a configured restriction should still flag the gated spec"
        );
        let FeatureGateViolation::Spec { url, name, .. } = &without_flag[0];
        assert_eq!("https://specs.apollo.dev/connect/v0.4", url);
        assert_eq!("Connect v0.4", name);

        let with_flag = violations(
            include_str!("testdata/connectv0_4.router.yaml"),
            &restrictions,
        );
        assert!(
            with_flag.is_empty(),
            "enabling the gate's config key should clear the violation"
        );
    }
}
