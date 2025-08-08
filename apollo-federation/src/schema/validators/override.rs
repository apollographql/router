//! Validation logic for `@override` directives in Apollo Federation schemas

use std::collections::HashMap;
use std::collections::HashSet;

use itertools::Itertools;
use regex::Regex;
use strsim::levenshtein;

use crate::bail;
use crate::error::CompositionError;
use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::error::SubgraphLocation;
use crate::merger::hints::HintCode;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;
use crate::supergraph::CompositionHint;

/// Validates all @override directives across subgraphs
pub(crate) fn validate_override_directives(
    subgraphs: &[Subgraph<Validated>],
    hints: &mut Vec<CompositionHint>,
) -> Result<(), Vec<CompositionError>> {
    // Collect all fields with @override directives across all subgraphs
    let mut override_fields: HashMap<FieldCoordinate, Vec<OverrideInfo>> = HashMap::new();
    let mut composition_errors: Vec<CompositionError> = Vec::new();

    for (subgraph_idx, subgraph) in subgraphs.iter().enumerate() {
        if let Err(e) = collect_override_fields(subgraph, subgraph_idx, &mut override_fields) {
            composition_errors.push(CompositionError::SubgraphError {
                subgraph: subgraph.name.clone(),
                error: SingleFederationError::InvalidSubgraph {
                    message: e.to_string(),
                },
                locations: Default::default(),
            });
        }
    }

    // First, validate that no field has multiple subgraphs claiming ownership
    validate_override_conflicts(subgraphs, &override_fields, &mut composition_errors);

    // Build a lookup of which subgraphs have overrides on which fields
    let mut subgraph_overrides: HashMap<String, HashSet<String>> = HashMap::new();
    for (field_coordinate, override_infos) in &override_fields {
        for override_info in override_infos {
            subgraph_overrides
                .entry(override_info.subgraph_name.clone())
                .or_default()
                .insert(field_coordinate.to_string());
        }
    }

    // Then validate each field that has @override directives
    for (field_coordinate, override_infos) in override_fields {
        if let Err(e) = validate_field_overrides(
            subgraphs,
            &field_coordinate.to_string(),
            &override_infos,
            &subgraph_overrides,
            &mut composition_errors,
            hints,
        ) {
            // Attribute error to the first subgraph that referenced this field
            let subgraph_name = override_infos
                .first()
                .map(|i| i.subgraph_name.clone())
                .unwrap_or_else(|| "unknown".to_string());
            composition_errors.push(CompositionError::SubgraphError {
                subgraph: subgraph_name,
                error: SingleFederationError::InvalidSubgraph {
                    message: e.to_string(),
                },
                locations: Default::default(),
            });
        }
    }

    if composition_errors.is_empty() {
        Ok(())
    } else {
        Err(composition_errors)
    }
}

#[derive(Debug, Clone)]
struct OverrideInfo {
    subgraph_idx: usize,
    subgraph_name: String,
    field_position: FieldDefinitionPosition,
    from_subgraph: String,
    label: Option<String>,
    is_interface_field: bool,
    is_interface_object: bool,
}

/// A field coordinate that avoids string allocations during collection
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct FieldCoordinate {
    type_name: String,
    field_name: String,
}

impl FieldCoordinate {
    fn new(type_name: impl Into<String>, field_name: impl Into<String>) -> Self {
        Self {
            type_name: type_name.into(),
            field_name: field_name.into(),
        }
    }
}

impl std::fmt::Display for FieldCoordinate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.type_name, self.field_name)
    }
}

fn collect_override_fields(
    subgraph: &Subgraph<Validated>,
    subgraph_idx: usize,
    override_fields: &mut HashMap<FieldCoordinate, Vec<OverrideInfo>>,
) -> Result<(), FederationError> {
    let schema = subgraph.schema();
    if let Ok(Some(directive_name)) = subgraph.override_directive_name() {
        let referencers = schema.referencers().get_directive(&directive_name)?;
        for field_pos in referencers.object_or_interface_fields() {
            // Build coordinate and field position
            let field_coordinate = FieldCoordinate::new(
                field_pos.type_name().to_string(),
                field_pos.field_name().to_string(),
            );
            let (field_position, is_interface_field, is_interface_object) = match field_pos.clone()
            {
                crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition::Object(ofp) => {
                    let is_interface_object = subgraph.is_interface_object_type(
                        &TypeDefinitionPosition::Object(ObjectTypeDefinitionPosition {
                            type_name: ofp.type_name.clone(),
                        }),
                    );
                    (
                        FieldDefinitionPosition::Object(ofp),
                        false,
                        is_interface_object,
                    )
                }
                crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition::Interface(
                    ifp,
                ) => (FieldDefinitionPosition::Interface(ifp), true, false),
            };

            // Fetch the actual directive instance from the field
            let comp = field_position.get(schema.schema())?;
            let field_def = &comp.node;
            let Some(override_directive) = field_def.directives.get("override") else {
                continue;
            };

            let override_info = extract_override_info(
                subgraph_idx,
                &subgraph.name,
                field_position,
                override_directive,
                is_interface_field,
                is_interface_object,
            )?;

            override_fields
                .entry(field_coordinate)
                .or_default()
                .push(override_info);
        }
    }

    Ok(())
}

/// Helper function to extract a string argument from a directive
fn extract_string_argument(
    directive: &apollo_compiler::ast::Directive,
    arg_name: &str,
) -> Option<String> {
    directive
        .arguments
        .iter()
        .find(|arg| arg.name == arg_name)?
        .value
        .as_ref()
        .as_str()
        .map(str::to_owned)
}

fn extract_override_info(
    subgraph_idx: usize,
    subgraph_name: &str,
    field_position: FieldDefinitionPosition,
    override_directive: &apollo_compiler::ast::Directive,
    is_interface_field: bool,
    is_interface_object: bool,
) -> Result<OverrideInfo, FederationError> {
    // Extract "from" argument - required
    let Some(from_subgraph) = extract_string_argument(override_directive, "from") else {
        bail!("@override directive missing 'from' argument");
    };

    // Extract optional "label" argument
    let label = extract_string_argument(override_directive, "label");

    Ok(OverrideInfo {
        subgraph_idx,
        subgraph_name: subgraph_name.to_string(),
        field_position,
        from_subgraph,
        label,
        is_interface_field,
        is_interface_object,
    })
}

fn validate_field_overrides(
    subgraphs: &[Subgraph<Validated>],
    field_coordinate: &str,
    override_infos: &[OverrideInfo],
    subgraph_overrides: &HashMap<String, HashSet<String>>,
    composition_errors: &mut Vec<CompositionError>,
    hints: &mut Vec<CompositionHint>,
) -> Result<(), FederationError> {
    for override_info in override_infos {
        // Check for interface field restriction
        if override_info.is_interface_field {
            push_comp_error(
                composition_errors,
                subgraphs,
                override_info,
                SingleFederationError::OverrideOnInterface {
                    message: format!(
                        "@override cannot be used on field \"{}\" on subgraph \"{}\": @override is not supported on interface type fields.",
                        field_coordinate, override_info.subgraph_name
                    ),
                },
            );
            continue;
        }

        // If source subgraph exists but no longer defines this field, emit a hint to remove @override
        if let Some(source_subgraph) = subgraphs
            .iter()
            .find(|s| s.name == override_info.from_subgraph)
        {
            let source_schema = source_subgraph.schema();
            let type_name = field_coordinate.split('.').next().unwrap_or("");
            let field_name = field_coordinate.split('.').nth(1).unwrap_or("");
            let mut source_field: Option<&apollo_compiler::ast::FieldDefinition> = None;
            let type_exists_and_has_field = source_schema.schema.types.values().any(|t| match t {
                apollo_compiler::schema::ExtendedType::Object(obj) => {
                    if obj.name.as_str() == type_name {
                        if let Some(fd) = obj.fields.get(field_name) {
                            source_field = Some(fd);
                            return true;
                        }
                    }
                    false
                }
                apollo_compiler::schema::ExtendedType::Interface(intf) => {
                    if intf.name.as_str() == type_name {
                        if let Some(fd) = intf.fields.get(field_name) {
                            source_field = Some(fd);
                            return true;
                        }
                    }
                    false
                }
                _ => false,
            });

            if !type_exists_and_has_field {
                hints.push(CompositionHint::new(
                    format!(
                        "Field \"{}\" on subgraph \"{}\" no longer exists in the from subgraph. The @override directive can be removed.",
                        field_coordinate, override_info.subgraph_name
                    ),
                    HintCode::OverrideDirectiveCanBeRemoved
                        .definition()
                        .code()
                        .to_string(),
                ));
                continue;
            }

            // If the source field exists, check for directive collisions and hints on the source side
            if let Some(src_field) = source_field {
                let src_has_external = src_field.directives.get("external").is_some();
                let src_has_provides = src_field.directives.get("provides").is_some();
                let src_has_requires = src_field.directives.get("requires").is_some();

                // Collision: @provides or @requires on source makes override invalid
                if src_has_provides || src_has_requires {
                    let conflicting = if src_has_provides {
                        "provides"
                    } else {
                        "requires"
                    };
                    push_comp_error(
                        composition_errors,
                        subgraphs,
                        override_info,
                        SingleFederationError::OverrideCollisionWithAnotherDirective {
                            message: format!(
                                "@override cannot be used on field \"{}\" on subgraph \"{}\" since \"{}\" on \"{}\" is marked with directive \"@{}\"",
                                field_coordinate,
                                override_info.subgraph_name,
                                field_coordinate,
                                override_info.from_subgraph,
                                conflicting
                            ),
                        },
                    );
                    continue;
                }

                // Hint: source marked external => can remove @override
                if src_has_external {
                    hints.push(CompositionHint::new(
                        format!(
                            "Field \"{}\" on subgraph \"{}\" is not resolved anymore by the from subgraph (it is marked \"@external\" in \"{}\"). The @override directive can be removed.",
                            field_coordinate, override_info.subgraph_name, override_info.from_subgraph
                        ),
                        HintCode::OverrideDirectiveCanBeRemoved
                            .definition()
                            .code()
                            .to_string(),
                    ));
                    // No continue; TS continues to possibly set labels but we've already handled label below
                }

                // Determine if source field is "used" (approximate):
                // - has @provides or @requires on the field
                // - OR the parent type has a @key that references this field name
                let mut src_is_used = src_has_provides || src_has_requires;
                if !src_is_used {
                    let type_name = field_coordinate.split('.').next().unwrap_or("");
                    let field_name = field_coordinate.split('.').nth(1).unwrap_or("");
                    if let Some(ty) = source_schema.schema.types.get(type_name) {
                        src_is_used = match ty {
                            apollo_compiler::schema::ExtendedType::Object(obj) => obj
                                .directives
                                .get_all("key")
                                .flat_map(|d| d.arguments.iter())
                                .any(|arg| {
                                    arg.name == "fields"
                                        && arg
                                            .value
                                            .as_str()
                                            .is_some_and(|s| s.contains(field_name))
                                }),
                            apollo_compiler::schema::ExtendedType::Interface(intf) => intf
                                .directives
                                .get_all("key")
                                .flat_map(|d| d.arguments.iter())
                                .any(|arg| {
                                    arg.name == "fields"
                                        && arg
                                            .value
                                            .as_str()
                                            .is_some_and(|s| s.contains(field_name))
                                }),
                            _ => false,
                        };
                    }
                }

                // Hint: if field is "used" and no label, suggest removal
                if src_is_used && override_info.label.is_none() {
                    hints.push(CompositionHint::new(
                        format!(
                            "Field \"{}\" on subgraph \"{}\" is overridden. It is still used in some federation directive(s) (@key, @requires, and/or @provides) and/or to satisfy interface constraint(s), but consider marking it @external explicitly or removing it along with its references.",
                            field_coordinate, override_info.from_subgraph
                        ),
                        HintCode::OverriddenFieldCanBeRemoved
                            .definition()
                            .code()
                            .to_string(),
                    ));
                }
            }
        }

        // Check for interface object restriction
        if override_info.is_interface_object {
            push_comp_error(
                composition_errors,
                subgraphs,
                override_info,
                SingleFederationError::OverrideCollisionWithAnotherDirective {
                    message: format!(
                        "@override is not yet supported on fields of @interfaceObject types: cannot be used on field \"{}\" on subgraph \"{}\".",
                        field_coordinate, override_info.subgraph_name
                    ),
                },
            );
            continue;
        }

        // Check for collision with @external on the destination field (align with TS behavior)
        if let Some(dest_subgraph) = subgraphs.get(override_info.subgraph_idx) {
            let schema = dest_subgraph.schema();
            if let Ok(comp) = override_info.field_position.get(schema.schema()) {
                let field_def = &comp.node;
                let has_external = field_def.directives.get("external").is_some();
                if has_external {
                    push_comp_error(
                        composition_errors,
                        subgraphs,
                        override_info,
                        SingleFederationError::OverrideCollisionWithAnotherDirective {
                            message: format!(
                                "@override cannot be used on field \"{}\" on subgraph \"{}\" since \"{}\" on \"{}\" is marked with directive \"@external\"",
                                field_coordinate,
                                override_info.subgraph_name,
                                field_coordinate,
                                override_info.subgraph_name
                            ),
                        },
                    );
                    continue;
                }
            }
        }

        // Check if source subgraph exists; if not, emit a hint (not an error)
        if !subgraphs
            .iter()
            .any(|s| s.name == override_info.from_subgraph)
        {
            let suggestions = suggest_similar_subgraph_names(
                &override_info.from_subgraph,
                subgraphs.iter().map(|s| s.name.as_str()),
            );
            let extra_msg = format_did_you_mean(&suggestions);
            hints.push(CompositionHint::new(
                format!(
                    "Source subgraph \"{}\" for field \"{}\" on subgraph \"{}\" does not exist{}",
                    override_info.from_subgraph,
                    field_coordinate,
                    override_info.subgraph_name,
                    extra_msg
                ),
                HintCode::FromSubgraphDoesNotExist
                    .definition()
                    .code()
                    .to_string(),
            ));
            continue;
        }

        // Check for self-override
        if override_info.from_subgraph == override_info.subgraph_name {
            push_comp_error(
                composition_errors,
                subgraphs,
                override_info,
                SingleFederationError::OverrideFromSelfError {
                    message: format!(
                        "Source and destination subgraphs \"{}\" are the same for overridden field \"{}\"",
                        override_info.from_subgraph, field_coordinate
                    ),
                },
            );
            continue;
        }

        // Check if the source subgraph also has an @override directive on the same field
        if let Some(source_overrides) = subgraph_overrides.get(&override_info.from_subgraph) {
            if source_overrides.contains(field_coordinate) {
                push_comp_error(
                    composition_errors,
                    subgraphs,
                    override_info,
                    SingleFederationError::OverrideSourceHasOverride {
                        message: format!(
                            "Field \"{}\" on subgraph \"{}\" is also marked with directive @override in subgraph \"{}\". A field cannot be overridden from a subgraph that also overrides the same field.",
                            field_coordinate,
                            override_info.subgraph_name,
                            override_info.from_subgraph
                        ),
                    },
                );
                continue;
            }
        }

        // Validate override label if present
        if let Some(ref label) = override_info.label {
            if !is_valid_override_label(label) {
                push_comp_error(
                    composition_errors,
                    subgraphs,
                    override_info,
                    SingleFederationError::OverrideLabelInvalid {
                        message: format!(
                            "Invalid @override label \"{}\" on field \"{}\" on subgraph \"{}\": labels must start with a letter and after that may contain alphanumerics, underscores, minuses, colons, periods, or slashes. Alternatively, labels may be of the form \"percent(x)\" where x is a float between 0-100 inclusive.",
                            label, field_coordinate, override_info.subgraph_name
                        ),
                    },
                );
            }
        }
    }

    Ok(())
}

/// Validates that only one subgraph claims ownership of each field via @override
fn validate_override_conflicts(
    subgraphs: &[Subgraph<Validated>],
    override_fields: &HashMap<FieldCoordinate, Vec<OverrideInfo>>,
    composition_errors: &mut Vec<CompositionError>,
) {
    for (field_coordinate, override_infos) in override_fields {
        // Check: Only ONE subgraph should claim ownership via @override
        if override_infos.len() > 1 {
            let claiming_subgraphs = override_infos
                .iter()
                .map(|info| info.subgraph_name.as_str())
                .join(", ");

            if let Some(first) = override_infos.first() {
                push_comp_error(
                    composition_errors,
                    subgraphs,
                    first,
                    SingleFederationError::DirectiveDefinitionInvalid {
                        message: format!(
                            "Field \"{}\" has multiple @override directives: subgraphs {} are all trying to claim ownership. Only one subgraph can override a field.",
                            field_coordinate, claiming_subgraphs
                        ),
                    },
                );
            }
        }
    }
}

fn push_comp_error(
    out: &mut Vec<CompositionError>,
    subgraphs: &[Subgraph<Validated>],
    info: &OverrideInfo,
    single: SingleFederationError,
) {
    let (locations, subgraph_name) = if let Some(sub) = subgraphs.get(info.subgraph_idx) {
        let schema = sub.schema();
        let locs = info
            .field_position
            .get(schema.schema())
            .ok()
            .map(|comp| {
                schema
                    .node_locations(comp)
                    .map(|range| SubgraphLocation {
                        subgraph: sub.name.clone(),
                        range,
                    })
                    .collect()
            })
            .unwrap_or_default();
        (locs, sub.name.clone())
    } else {
        (Vec::new(), info.subgraph_name.clone())
    };

    out.push(CompositionError::SubgraphError {
        subgraph: subgraph_name,
        error: single,
        locations,
    });
}

/// Validates the format of an override label
fn is_valid_override_label(
    label: &str
) -> bool {
    static LABEL_REGEX: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^[a-zA-Z][a-zA-Z0-9_\-:\./]*$").expect("valid label regex")
    });
    static PERCENT_REGEX: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^percent\((\d{1,2}(\.\d{1,8})?|100)\)$").expect("valid percent regex")
    });
    // Check if it matches the alphanumeric pattern
    if LABEL_REGEX.is_match(label) {
        return true;
    }
    // Check if it matches the percent pattern
    if let Some(captures) = PERCENT_REGEX.captures(label) {
        if let Some(percent_str) = captures.get(1) {
            if let Ok(percent_value) = percent_str.as_str().parse::<f64>() {
                if (0.0..=100.0).contains(&percent_value) {
                    return true;
                }
            }
        }
    }
    // Invalid label format
    false
}

fn suggest_similar_subgraph_names<'a, I>(target: &str, available: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    // GraphQL suggestionList/didYouMean cutoff: floor(len * 0.4) + 1 edits
    let max_dist = ((target.len() as f64) * 0.4).floor() as usize + 1;
    let target_lower = target.to_lowercase();

    let mut candidates: Vec<(String, usize)> = available
        .into_iter()
        .filter_map(|subgraph| {
            // Case-only differences are considered best suggestions
            if target_lower == subgraph.to_lowercase() {
                return Some((subgraph.to_string(), 0));
            }
            let dist = levenshtein(target, subgraph);
            if dist <= max_dist {
                Some((subgraph.to_string(), dist))
            } else {
                None
            }
        })
        .collect();

    // Sort by increasing distance, then alphabetically for ties
    candidates.sort_by(|(a_name, a_dist), (b_name, b_dist)| {
        a_dist.cmp(b_dist).then_with(|| a_name.cmp(b_name))
    });

    // Return up to 5 suggestions
    candidates.into_iter().take(5).map(|(s, _)| s).collect()
}

fn format_did_you_mean(suggestions: &[String]) -> String {
    match suggestions {
        [] => String::new(),
        [only] => format!(" Did you mean \"{}\"?", only),
        _ => format!(" Did you mean one of: {}?", suggestions.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subgraph::typestate::Subgraph;
    use crate::subgraph::typestate::Validated;

    fn build_subgraph_with_name(name: &str, schema_str: &str) -> Subgraph<Validated> {
        let subgraph =
            Subgraph::parse(name, &format!("http://{name}"), schema_str).expect("valid schema");
        let mut subgraph = subgraph
            .expand_links()
            .expect("expand links")
            .assume_upgraded();
        subgraph
            .normalize_root_types()
            .expect("normalize root types");
        subgraph.validate().expect("validate subgraph")
    }

    #[test]
    fn test_override_destination_collides_with_external() {
        let source_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override", "@external", "@requires"])           
            type User @key(fields: "id") {
                id: ID!
                name: String
            }
        "#;
        let dest_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override", "@external", "@requires"])           
            type User @key(fields: "id") {
                id: ID!
                name: String @override(from: "source") @external
                other: String @requires(fields: "name")
            }
        "#;

        let source = build_subgraph_with_name("source", source_schema);
        let dest = build_subgraph_with_name("dest", dest_schema);

        let mut hints = Vec::new();
        let result = validate_override_directives(&[source, dest], &mut hints);
        assert!(result.is_err());
        let errs = result.err().unwrap();
        assert!(errs.iter().any(|e| matches!(
            e,
            CompositionError::SubgraphError {
                error: SingleFederationError::OverrideCollisionWithAnotherDirective { .. },
                ..
            }
        )));
        assert!(hints.is_empty());
    }

    #[test]
    fn test_override_source_external_hints_can_be_removed() {
        let source_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override", "@external", "@requires"])           
            type User @key(fields: "id") {
                id: ID!
                name: String @external
                other: String @requires(fields: "name")
            }
        "#;
        let dest_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override", "@external"])           
            type User @key(fields: "id") {
                id: ID!
                name: String @override(from: "source")
            }
        "#;

        let source = build_subgraph_with_name("source", source_schema);
        let dest = build_subgraph_with_name("dest", dest_schema);

        let mut hints = Vec::new();
        let result = validate_override_directives(&[source, dest], &mut hints);
        assert!(result.is_ok());
        assert!(
            hints
                .iter()
                .any(|h| h.code() == HintCode::OverrideDirectiveCanBeRemoved.code())
        );
    }

    #[test]
    fn test_override_source_provides_or_requires_emits_error() {
        let source_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override", "@requires", "@external"])           
            type User @key(fields: "id") {
                id: ID! @external
                name: String @requires(fields: "id")
            }
        "#;
        let dest_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])           
            type User @key(fields: "id") {
                id: ID!
                name: String @override(from: "source")
            }
        "#;

        let source = build_subgraph_with_name("source", source_schema);
        let dest = build_subgraph_with_name("dest", dest_schema);

        let mut hints = Vec::new();
        let result = validate_override_directives(&[source, dest], &mut hints);
        assert!(result.is_err());
        let errs = result.err().unwrap();
        assert!(errs.iter().any(|e| matches!(
            e,
            CompositionError::SubgraphError {
                error: SingleFederationError::OverrideCollisionWithAnotherDirective { .. },
                ..
            }
        )));
        assert!(hints.is_empty());
    }

    #[test]
    fn test_override_source_used_without_label_emits_hint() {
        let source_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])           
            type User @key(fields: "id name") {
                id: ID!
                name: String
            }
        "#;
        let dest_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])           
            type User @key(fields: "id") {
                id: ID!
                name: String @override(from: "source")
            }
        "#;

        let source = build_subgraph_with_name("source", source_schema);
        let dest = build_subgraph_with_name("dest", dest_schema);

        let mut hints = Vec::new();
        let _ = validate_override_directives(&[source, dest], &mut hints);
        assert!(
            hints
                .iter()
                .any(|h| h.code() == HintCode::OverriddenFieldCanBeRemoved.code())
        );
    }

    #[test]
    fn test_validate_override_directives_complex_happy_path() {
        // Test multiple complex subgraphs that can work together with @override directives

        // Users subgraph - defines User entity with basic fields
        let users_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])
                
            type User @key(fields: "id") {
                id: ID!
                email: String
                createdAt: String
            }
        "#;

        // Accounts subgraph - originally owns User.name, but it's being overridden by profiles
        let accounts_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])
                
            type User @key(fields: "id") {
                id: ID!
                name: String
                accountStatus: String
            }
        "#;

        // Profiles subgraph - overrides User.name from accounts, adds displayName
        let profiles_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])
                
            type User @key(fields: "id") {
                id: ID!
                name: String @override(from: "accounts")
                displayName: String
                bio: String
            }
        "#;

        let users_subgraph = build_subgraph_with_name("users", users_schema);
        let accounts_subgraph = build_subgraph_with_name("accounts", accounts_schema);
        let profiles_subgraph = build_subgraph_with_name("profiles", profiles_schema);

        let result = {
            let mut hints = Vec::new();
            validate_override_directives(
                &[users_subgraph, accounts_subgraph, profiles_subgraph],
                &mut hints,
            )
        };

        // Should succeed with no validation errors - this is a valid override scenario
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_override_directives_complex_conflict_scenarios() {
        // Test multiple complex subgraphs that CANNOT work together - should detect various conflicts

        // Profiles subgraph - tries to override User.name from accounts
        let profiles_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])
                
            type User @key(fields: "id") {
                id: ID!
                name: String @override(from: "accounts")
                displayName: String
            }
        "#;

        // Social subgraph - ALSO tries to override User.name from accounts (CONFLICT!)
        let social_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])
                
            type User @key(fields: "id") {
                id: ID!
                name: String @override(from: "accounts")
                followers: Int
            }
        "#;

        // Accounts subgraph - the source that both are trying to override from
        let accounts_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])
                
            type User @key(fields: "id") {
                id: ID!
                name: String
                accountStatus: String
            }
        "#;

        let profiles_subgraph = build_subgraph_with_name("profiles", profiles_schema);
        let social_subgraph = build_subgraph_with_name("social", social_schema);
        let accounts_subgraph = build_subgraph_with_name("accounts", accounts_schema);

        let result = {
            let mut hints = Vec::new();
            validate_override_directives(
                &[profiles_subgraph, social_subgraph, accounts_subgraph],
                &mut hints,
            )
        };

        // Should fail and return validation errors
        assert!(result.is_err());
        let error_messages: Vec<String> = result
            .err()
            .unwrap()
            .iter()
            .map(|e| e.to_string())
            .collect();
        assert!(
            error_messages.iter().any(|msg| {
                msg.contains("multiple @override directives")
                    && msg.contains("User.name")
                    && (msg.contains("profiles") || msg.contains("social"))
                    && msg.contains("Only one subgraph can override a field")
            }),
            "Expected multiple override conflict error, got: {:?}",
            error_messages
        );
    }

    #[test]
    fn test_validate_override_directives_source_subgraph_has_override() {
        // Test chain override scenario: SubgraphA -> SubgraphB -> SubgraphC (invalid)

        // SubgraphC - original owner of User.name
        let subgraph_c_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])
                
            type User @key(fields: "id") {
                id: ID!
                name: String
                originalField: String
            }
        "#;

        // SubgraphB - overrides User.name from SubgraphC
        let subgraph_b_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])
                
            type User @key(fields: "id") {
                id: ID!
                name: String @override(from: "subgraph_c")
                middleField: String
            }
        "#;

        // SubgraphA - tries to override User.name from SubgraphB (INVALID: source also has override)
        let subgraph_a_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])
                
            type User @key(fields: "id") {
                id: ID!
                name: String @override(from: "subgraph_b")
                finalField: String
            }
        "#;

        let subgraph_a = build_subgraph_with_name("subgraph_a", subgraph_a_schema);
        let subgraph_b = build_subgraph_with_name("subgraph_b", subgraph_b_schema);
        let subgraph_c = build_subgraph_with_name("subgraph_c", subgraph_c_schema);

        let result = {
            let mut hints = Vec::new();
            validate_override_directives(&[subgraph_a, subgraph_b, subgraph_c], &mut hints)
        };

        // Should fail and return validation errors
        assert!(result.is_err());
        // Should detect the source subgraph also has override error
        let error_messages: Vec<String> = result
            .err()
            .unwrap()
            .iter()
            .map(|e| e.to_string())
            .collect();
        assert!(
            error_messages.iter().any(|msg| {
                msg.contains("subgraph_b")
                    && msg.contains("also marked with directive @override")
                    && msg.contains("cannot be overridden from a subgraph that also overrides")
            }),
            "Expected source subgraph has override error, got: {:?}",
            error_messages
        );
    }

    #[test]
    fn test_validate_override_directives_self_override_error() {
        // Test self-override scenario: subgraph trying to override from itself

        // Users subgraph - tries to override User.name from itself (INVALID)
        let users_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])
                
            type User @key(fields: "id") {
                id: ID!
                name: String @override(from: "users")
                email: String
            }
        "#;

        // Accounts subgraph - normal subgraph
        let accounts_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])
                
            type User @key(fields: "id") {
                id: ID!
                accountStatus: String
            }
        "#;

        let users_subgraph = build_subgraph_with_name("users", users_schema);
        let accounts_subgraph = build_subgraph_with_name("accounts", accounts_schema);

        let result = {
            let mut hints = Vec::new();
            validate_override_directives(&[users_subgraph, accounts_subgraph], &mut hints)
        };

        // Should fail and return validation errors
        assert!(result.is_err());
        // Should detect the self-override error
        let error_messages: Vec<String> = result
            .err()
            .unwrap()
            .iter()
            .map(|e| e.to_string())
            .collect();
        assert!(
            error_messages.iter().any(|msg| {
                msg.contains("Source and destination subgraphs \"users\" are the same")
                    && msg.contains("User.name")
            }),
            "Expected self-override error, got: {:?}",
            error_messages
        );
    }

    #[test]
    fn test_validate_override_directives_nonexistent_source_subgraph() {
        // Test nonexistent source subgraph scenario

        // Users subgraph - tries to override from nonexistent subgraph
        let users_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])
                
            type User @key(fields: "id") {
                id: ID!
                name: String @override(from: "nonexistent_subgraph")
                email: String
            }
        "#;

        // Accounts subgraph - real subgraph that exists
        let accounts_schema = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@key", "@override"])
                
            type User @key(fields: "id") {
                id: ID!
                accountStatus: String
            }
        "#;

        let users_subgraph = build_subgraph_with_name("users", users_schema);
        let accounts_subgraph = build_subgraph_with_name("accounts", accounts_schema);

        let mut hints = Vec::new();
        let result = validate_override_directives(&[users_subgraph, accounts_subgraph], &mut hints);

        // Should succeed without errors but produce a hint
        assert!(result.is_ok());
        assert!(
            hints
                .iter()
                .any(|h| h.code() == HintCode::FromSubgraphDoesNotExist.code()),
            "Expected FromSubgraphDoesNotExist hint, got: {:?}",
            hints
        );
    }
}
