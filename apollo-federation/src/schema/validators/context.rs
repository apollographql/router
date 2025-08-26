use std::collections::HashMap;

use apollo_compiler::Name;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::schema::FederationSchema;

pub(crate) fn validate_context_directives(
    schema: &FederationSchema,
    errors: &mut MultipleFederationErrors,
) -> Result<HashMap<String, Vec<Name>>, FederationError> {
    let context_rules: Vec<Box<dyn ContextValidator>> = vec![
        Box::new(DenyUnderscoreInContextName::new()),
        Box::new(DenyInvalidContextName::new()),
    ];

    let mut context_to_type_map: HashMap<String, Vec<Name>> = HashMap::new();

    let Ok(context_directives) = schema.context_directive_applications() else {
        // if we get an error, we probably are pre fed 2.8
        return Ok(context_to_type_map);
    };
    for context_directive in context_directives {
        match context_directive {
            Ok(context) => {
                let name = context.arguments.name.to_string();

                // Apply each validation rule
                for rule in context_rules.iter() {
                    rule.validate(&name, errors);
                }

                // Track which types use each context name
                let types = context_to_type_map.entry(name).or_default();
                types.push(context.target.type_name().clone());
            }
            Err(e) => errors.push(e),
        }
    }
    Ok(context_to_type_map)
}

/// Trait for context name validators
trait ContextValidator {
    fn validate(&self, context_name: &str, errors: &mut MultipleFederationErrors);
}

/// Validator that ensures context names don't contain underscores
struct DenyUnderscoreInContextName {}

impl DenyUnderscoreInContextName {
    fn new() -> Self {
        Self {}
    }
}

impl ContextValidator for DenyUnderscoreInContextName {
    fn validate(&self, context_name: &str, errors: &mut MultipleFederationErrors) {
        if context_name.contains('_') {
            errors.push(
                SingleFederationError::ContextNameContainsUnderscore {
                    name: context_name.to_string(),
                }
                .into(),
            );
        }
    }
}

/// Validator that ensures context names only contain valid alphanumeric characters
/// and start with a letter
struct DenyInvalidContextName {}

impl DenyInvalidContextName {
    fn new() -> Self {
        Self {}
    }
}

impl ContextValidator for DenyInvalidContextName {
    fn validate(&self, context_name: &str, errors: &mut MultipleFederationErrors) {
        if !context_name.chars().all(|c| c.is_alphanumeric())
            || !context_name
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic())
        {
            errors.push(
                SingleFederationError::ContextNameInvalid {
                    name: context_name.to_string(),
                }
                .into(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_underscore_in_context_name() {
        let mut errors = MultipleFederationErrors::new();
        let rule = DenyUnderscoreInContextName::new();

        rule.validate("invalid_name", &mut errors);

        assert_eq!(errors.errors.len(), 1);
        assert!(
            matches!(
                errors.errors[0].clone(),
                SingleFederationError::ContextNameContainsUnderscore { name } if name == "invalid_name"
            ),
            "Expected an error about underscore in context name, but got: {:?}",
            errors.errors[0]
        );

        // Test valid case
        let mut errors = MultipleFederationErrors::new();
        rule.validate("validName", &mut errors);
        assert_eq!(errors.errors.len(), 0, "Expected no errors for valid name");
    }

    #[test]
    fn deny_invalid_context_name() {
        let mut errors = MultipleFederationErrors::new();
        let rule = DenyInvalidContextName::new();

        // Test name starting with number
        rule.validate("123invalid", &mut errors);

        assert_eq!(errors.errors.len(), 1);
        assert!(
            matches!(
                errors.errors[0].clone(),
                SingleFederationError::ContextNameInvalid { name } if name == "123invalid"
            ),
            "Expected an error about invalid context name, but got: {:?}",
            errors.errors[0]
        );

        // Test name with special characters
        let mut errors = MultipleFederationErrors::new();
        rule.validate("invalid$name", &mut errors);

        assert_eq!(errors.errors.len(), 1);
        assert!(
            matches!(
                errors.errors[0].clone(),
                SingleFederationError::ContextNameInvalid { name } if name == "invalid$name"
            ),
            "Expected an error about invalid context name, but got: {:?}",
            errors.errors[0]
        );

        // Test valid case
        let mut errors = MultipleFederationErrors::new();
        rule.validate("validName123", &mut errors);
        assert_eq!(errors.errors.len(), 0, "Expected no errors for valid name");
    }
}
