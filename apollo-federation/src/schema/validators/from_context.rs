use apollo_compiler::Name;
use std::collections::HashMap;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::schema::FederationSchema;

pub(crate) fn validate_from_context_directives(
    schema: &FederationSchema,
    context_map: &HashMap<String, Vec<Name>>,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let from_context_rules: Vec<Box<dyn FromContextValidator>> = vec![
        Box::new(DenyOnDirectiveDefinition::new()),
        Box::new(DenyOnAbstractType::new()),
        Box::new(DenyOnInterfaceImplementation::new()),
        Box::new(DenyWithDefaultValue::new()),
        Box::new(RequireContextExists::new(context_map.clone())),
        Box::new(RequireResolvableKey::new()),
    ];

    for from_context_directive in schema.from_context_directive_applications()? {
        match from_context_directive {
            Ok(from_context) => {
                // Parse context and selection from the field value
                let field = from_context.arguments.field.to_string();
                let (context, selection) = parse_context(&field);
                
                // Apply each validation rule
                for rule in from_context_rules.iter() {
                    rule.validate(&from_context, &context, &selection, errors);
                }
                
                // TODO: Add validate_field_value when needed
            }
            Err(e) => errors.push(e),
        }
    }
    
    Ok(())
}

fn parse_context(field: &str) -> (String, String) {
    // Split the context reference into context name and selection path
    let parts: Vec<&str> = field.splitn(2, '.').collect();
    if parts.len() == 2 {
        (parts[0].to_string(), parts[1].to_string())
    } else {
        (String::new(), String::new())
    }
}

fn validate_field_value(
    _context: &str,
    _selection: &str,
    _from_context: &impl FromContextDirectiveApplication,
    _set_context_locations: &[Name],
    _schema: &FederationSchema,
    _errors: &mut MultipleFederationErrors,
) {
    // TODO: Implement field value validation
    todo!("Implement validateFieldValue");
}

/// Trait for accessing properties of @fromContext directive applications
trait FromContextDirectiveApplication {
    fn target_coordinate(&self) -> String;
    fn is_on_directive_definition(&self) -> bool;
    fn is_on_abstract_type(&self) -> bool;
    fn implements_interface_field(&self) -> bool;
    fn has_default_value(&self) -> bool;
    fn has_resolvable_key(&self) -> bool;
    fn object_type_name(&self) -> Name;
}

/// Trait for @fromContext directive validators
trait FromContextValidator {
    fn validate(
        &self, 
        from_context: &impl FromContextDirectiveApplication,
        context: &str,
        selection: &str,
        errors: &mut MultipleFederationErrors
    );
}

/// Validator that denies @fromContext on directive definitions
struct DenyOnDirectiveDefinition {}

impl DenyOnDirectiveDefinition {
    fn new() -> Self {
        Self {}
    }
}

impl FromContextValidator for DenyOnDirectiveDefinition {
    fn validate(
        &self,
        from_context: &impl FromContextDirectiveApplication,
        _context: &str,
        _selection: &str,
        errors: &mut MultipleFederationErrors,
    ) {
        if from_context.is_on_directive_definition() {
            errors.push(
                SingleFederationError::ContextNotSet {
                    message: format!(
                        "@fromContext argument cannot be used on a directive definition \"{}\".",
                        from_context.target_coordinate()
                    ),
                }
                .into(),
            );
        }
    }
}

/// Validator that denies @fromContext on abstract types
struct DenyOnAbstractType {}

impl DenyOnAbstractType {
    fn new() -> Self {
        Self {}
    }
}

impl FromContextValidator for DenyOnAbstractType {
    fn validate(
        &self,
        from_context: &impl FromContextDirectiveApplication,
        _context: &str,
        _selection: &str,
        errors: &mut MultipleFederationErrors,
    ) {
        if from_context.is_on_abstract_type() {
            errors.push(
                SingleFederationError::ContextNotSet {
                    message: format!(
                        "@fromContext argument cannot be used on a field that exists on an abstract type \"{}\".",
                        from_context.target_coordinate()
                    ),
                }
                .into(),
            );
        }
    }
}

/// Validator that denies @fromContext on fields implementing an interface
struct DenyOnInterfaceImplementation {}

impl DenyOnInterfaceImplementation {
    fn new() -> Self {
        Self {}
    }
}

impl FromContextValidator for DenyOnInterfaceImplementation {
    fn validate(
        &self,
        from_context: &impl FromContextDirectiveApplication,
        _context: &str,
        _selection: &str,
        errors: &mut MultipleFederationErrors,
    ) {
        if from_context.implements_interface_field() {
            errors.push(
                SingleFederationError::ContextNotSet {
                    message: format!(
                        "@fromContext argument cannot be used on a field implementing an interface field \"{}\".",
                        from_context.target_coordinate()
                    ),
                }
                .into(),
            );
        }
    }
}

/// Validator that denies @fromContext arguments with default values
struct DenyWithDefaultValue {}

impl DenyWithDefaultValue {
    fn new() -> Self {
        Self {}
    }
}

impl FromContextValidator for DenyWithDefaultValue {
    fn validate(
        &self,
        from_context: &impl FromContextDirectiveApplication,
        _context: &str,
        _selection: &str,
        errors: &mut MultipleFederationErrors,
    ) {
        if from_context.has_default_value() {
            errors.push(
                SingleFederationError::ContextNotSet {
                    message: format!(
                        "@fromContext arguments may not have a default value: \"{}\".",
                        from_context.target_coordinate()
                    ),
                }
                .into(),
            );
        }
    }
}

/// Validator that checks if the referenced context exists
struct RequireContextExists {
    context_map: HashMap<String, Vec<Name>>,
}

impl RequireContextExists {
    fn new(context_map: HashMap<String, Vec<Name>>) -> Self {
        Self { context_map }
    }
}

impl FromContextValidator for RequireContextExists {
    fn validate(
        &self,
        from_context: &impl FromContextDirectiveApplication,
        context: &str,
        selection: &str,
        errors: &mut MultipleFederationErrors,
    ) {
        if context.is_empty() || selection.is_empty() {
            errors.push(
                SingleFederationError::NoContextInSelection {
                    message: format!(
                        "@fromContext argument does not reference a context \"{}.{}\".",
                        context, selection
                    ),
                }
                .into(),
            );
        } else if !self.context_map.contains_key(context) {
            errors.push(
                SingleFederationError::ContextNotSet {
                    message: format!(
                        "Context \"{}\" is used at location \"{}\" but is never set.",
                        context,
                        from_context.target_coordinate()
                    ),
                }
                .into(),
            );
        }
    }
}

/// Validator that requires at least one resolvable key on the type
struct RequireResolvableKey {}

impl RequireResolvableKey {
    fn new() -> Self {
        Self {}
    }
}

impl FromContextValidator for RequireResolvableKey {
    fn validate(
        &self,
        from_context: &impl FromContextDirectiveApplication,
        _context: &str,
        _selection: &str,
        errors: &mut MultipleFederationErrors,
    ) {
        if !from_context.has_resolvable_key() {
            errors.push(
                SingleFederationError::ContextNoResolvableKey {
                    message: format!(
                        "Object \"{}\" has no resolvable key but has a field with a contextual argument.",
                        from_context.object_type_name()
                    ),
                }
                .into(),
            );
        }
    }
}