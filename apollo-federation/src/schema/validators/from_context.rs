use apollo_compiler::Name;
use std::collections::HashMap;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::schema::position::FieldArgumentDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::FederationSchema;
use crate::utils::FallibleIterator;

pub(crate) fn validate_from_context_directives(
    schema: &FederationSchema,
    context_map: &HashMap<String, Vec<Name>>,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let from_context_rules: Vec<Box<dyn FromContextValidator>> = vec![
        Box::new(DenyOnAbstractType::new()),
        Box::new(DenyOnInterfaceImplementation::new()),
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
                    rule.validate(&from_context.target, schema, &context, &selection, errors)?;
                }
                
                // TODO: Add validate_field_value when needed
            }
            Err(e) => errors.push(e),
        }
    }
    
    Ok(())
}

// TODO: Make this match the regex from JS
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
    _target: &FieldArgumentDefinitionPosition,
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
        target: &FieldArgumentDefinitionPosition,
        schema: &FederationSchema,
        context: &str,
        selection: &str,
        errors: &mut MultipleFederationErrors
    ) -> Result<(), FederationError>;
}

/// Validator that denies @fromContext on directive definitions
struct DenyOnDirectiveDefinition {}

impl DenyOnDirectiveDefinition {
    fn new() -> Self {
        Self {}
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
        target: &FieldArgumentDefinitionPosition,
        _schema: &FederationSchema,
        _context: &str,
        _selection: &str,
        errors: &mut MultipleFederationErrors,
    ) -> Result<(), FederationError> {
        match target {
            FieldArgumentDefinitionPosition::Interface(_) => {
                errors.push(
                SingleFederationError::ContextNotSet {
                    message: format!(
                        "@fromContext argument cannot be used on a field that exists on an abstract type \"{}\".",
                        as_coordinate(target)
                    ),
                    }
                    .into(),
                );
            }
            _ => {}
        }
        Ok(())
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
        target: &FieldArgumentDefinitionPosition,
        schema: &FederationSchema,
        _context: &str,
        _selection: &str,
        errors: &mut MultipleFederationErrors,
    ) -> Result<(), FederationError> {
        match target {
            FieldArgumentDefinitionPosition::Object(position) => {
                let obj = position.parent().parent().get(schema.schema())?;
                let field = position.parent().field_name;
                for implemented in &obj.implements_interfaces {
                    let itf = InterfaceTypeDefinitionPosition { type_name: implemented.name.clone() };
                    let field = itf.fields(schema.schema())?.find(|f| f.field_name == field);
                    if field.is_some() {
                        errors.push(
                            SingleFederationError::ContextNotSet {
                                message: format!(
                                    "@fromContext argument cannot be used on a field implementing an interface field \"{}\".",
                                    as_coordinate(target)
                                ),
                            }
                            .into(),
                        );
                    }
                }
            },
            _ => {}
        }
        Ok(())
    }
}

/// Validator that denies @fromContext arguments with default values
struct DenyWithDefaultValue {}

impl DenyWithDefaultValue {
    fn new() -> Self {
        Self {}
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
        target: &FieldArgumentDefinitionPosition,
        _schema: &FederationSchema,
        context: &str,
        selection: &str,
        errors: &mut MultipleFederationErrors,
    ) -> Result<(), FederationError> {
        if context.is_empty() || selection.is_empty() {
            errors.push(
                SingleFederationError::NoContextInSelection {
                    message: format!(
                        "@fromContext argument does not reference a context \"${} {}\".",
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
                        as_coordinate(target)
                    ),
                }
                .into(),
            );
        }
        Ok(())
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
        target: &FieldArgumentDefinitionPosition,
        schema: &FederationSchema,
        _context: &str,
        _selection: &str,
        errors: &mut MultipleFederationErrors,
    ) -> Result<(), FederationError> {
        match target {
            FieldArgumentDefinitionPosition::Object(position) => {
                let parent = position.parent().parent();
                if let Some(metadata) = &schema.subgraph_metadata {
                    let key_directive = metadata.federation_spec_definition().key_directive_definition(schema)?;
                    let keys_on_type = parent.get_applied_directives(schema, &key_directive.name);
                    if !keys_on_type.iter().fallible_filter(|application| -> Result<bool, FederationError> {
                        let arguments = metadata.federation_spec_definition().key_directive_arguments(application)?;
                        Ok(arguments.resolvable)
                    }).collect::<Result<Vec<_>, _>>()?.is_empty() {
                        errors.push(
                            SingleFederationError::ContextNoResolvableKey {
                                message: format!(
                                    "Object \"{}\" has no resolvable key but has a field with a contextual argument.",
                                    as_coordinate(target)
                                ),
                            }
                            .into(),
                        );
                    }
                }
            },
            _ => {}
        }
        Ok(())
    }
}

fn as_coordinate(target: &FieldArgumentDefinitionPosition) -> String {
    match target {
        FieldArgumentDefinitionPosition::Object(position) => {
            format!("{}.{}", position.type_name, position.field_name)
        }
        FieldArgumentDefinitionPosition::Interface(position) => {
            format!("{}.{}", position.type_name, position.field_name)
        }
    }
}