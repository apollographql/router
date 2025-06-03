use std::sync::LazyLock;

use regex::Regex;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::schema::FederationSchema;
use crate::schema::position::TagDirectiveTargetPosition;

/// Regex pattern that matches valid tag names: starts with underscore or letter,
/// followed by any combination of hyphens, underscores, forward slashes, digits, or letters
static TAG_NAME_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[_A-Za-z][-/_0-9A-Za-z]*$").expect("Invalid regex pattern"));

const MAX_TAG_LENGTH: usize = 128;

// TODO: Move this into the Position module
fn get_position_coordinate(position: &TagDirectiveTargetPosition) -> String {
    match position {
        TagDirectiveTargetPosition::ObjectField(pos) => {
            format!("{}.{}", pos.type_name, pos.field_name)
        }
        TagDirectiveTargetPosition::InterfaceField(pos) => {
            format!("{}.{}", pos.type_name, pos.field_name)
        }
        TagDirectiveTargetPosition::UnionField(pos) => {
            format!("{}.{}", pos.type_name, pos.field_name())
        }
        TagDirectiveTargetPosition::Object(pos) => pos.type_name.to_string(),
        TagDirectiveTargetPosition::Interface(pos) => pos.type_name.to_string(),
        TagDirectiveTargetPosition::Union(pos) => pos.type_name.to_string(),
        TagDirectiveTargetPosition::ArgumentDefinition(pos) => {
            format!(
                "{}.{}({}:)",
                pos.type_name(),
                pos.field_name(),
                pos.argument_name()
            )
        }
        TagDirectiveTargetPosition::Scalar(pos) => pos.type_name.to_string(),
        TagDirectiveTargetPosition::Enum(pos) => pos.type_name.to_string(),
        TagDirectiveTargetPosition::EnumValue(pos) => {
            format!("{}.{}", pos.type_name, pos.value_name)
        }
        TagDirectiveTargetPosition::InputObject(pos) => pos.type_name.to_string(),
        TagDirectiveTargetPosition::InputObjectFieldDefinition(pos) => {
            format!("{}.{}", pos.type_name, pos.field_name)
        }
        TagDirectiveTargetPosition::Schema(_) => "Schema".to_string(),
    }
}

pub(crate) fn validate_tag_directives(
    schema: &FederationSchema,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let tag_applications = schema.tag_directive_applications()?;

    for tag_directive_result in tag_applications {
        let tag_directive = match tag_directive_result {
            Ok(directive) => directive,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };

        let tag_name = tag_directive.arguments.name;
        let coordinate = get_position_coordinate(&tag_directive.target);

        // Validate tag name length and pattern
        if tag_name.len() > MAX_TAG_LENGTH || !TAG_NAME_PATTERN.is_match(tag_name) {
            let message = if matches!(tag_directive.target, TagDirectiveTargetPosition::Schema(_)) {
                format!(
                    "Schema root has invalid @tag directive value '{}' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores.",
                    tag_name
                )
            } else {
                format!(
                    "Schema element {} has invalid @tag directive value '{}' for argument \"name\". Values must start with an alphanumeric character or underscore and contain only slashes, hyphens, or underscores.",
                    coordinate, tag_name
                )
            };
            errors.push(SingleFederationError::InvalidTagName { message }.into());
        }
    }

    Ok(())
}
