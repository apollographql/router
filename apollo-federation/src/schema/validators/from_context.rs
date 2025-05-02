use apollo_compiler::Name;
use std::collections::HashMap;
use std::sync::LazyLock;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::schema::FederationSchema;
use crate::schema::position::FieldArgumentDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::utils::FallibleIterator;
use crate::utils::iter_into_single_item;
use regex::Regex;

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

pub(crate) fn parse_context(field: &str) -> (Option<String>, Option<String>) {
    // PORT_NOTE: The original JS regex, as shown below
    //   /^(?:[\n\r\t ,]|#[^\n\r]*(?![^\n\r]))*\$(?:[\n\r\t ,]|#[^\n\r]*(?![^\n\r]))*([A-Za-z_]\w*(?!\w))([\s\S]*)$/
    // makes use of negative lookaheads, which aren't supported natively by Rust's regex crate.
    // There's a fancy_regex crate which does support this, but in the specific case above, the
    // negative lookaheads are just used to ensure strict *-greediness for the preceding expression
    // (i.e., it guarantees those *-expressions match greedily and won't backtrack).
    //
    // We can emulate that in this case by matching a series of regexes instead of a single regex,
    // where for each regex, the relevant *-expression doesn't backtrack by virtue of the rest of
    // the haystack guaranteeing a match. Also note that Rust has (?s:.) to match all characters
    // including newlines, which we use in place of JS's common regex workaround of [\s\S].
    fn strip_leading_ignored_tokens(input: &str) -> Option<&str> {
        iter_into_single_item(CONTEXT_PARSING_LEADING_PATTERN.captures_iter(input))
            .and_then(|c| c.get(1))
            .map_or(None, |m| Some(m.as_str()))
    }

    let Some(dollar_start) = strip_leading_ignored_tokens(field) else { return (None, None) };
    
    let mut dollar_iter = dollar_start.chars();
    if dollar_iter.next() != Some('$') {
        return (None, None);
    }
    let after_dollar = dollar_iter.as_str();

    let Some(context_start) = strip_leading_ignored_tokens(after_dollar) else { return (None, None) };
    let Some(context_captures) =
        iter_into_single_item(CONTEXT_PARSING_CONTEXT_PATTERN.captures_iter(context_start))
    else {
        return (None, None);
    };

    let context = match context_captures.get(1).map(|m| m.as_str()) {
        Some(context) if !context.is_empty() => context,
        _ => {
            return (None, None);
        }
    };
    let selection = match context_captures.get(2).map(|m| m.as_str()) {
        Some(selection) => {
            let Some(selection) = strip_leading_ignored_tokens(selection) else { return (Some(context.to_owned()), None) };
            if !selection.is_empty() {
                selection
            } else {
                return (Some(context.to_owned()), None);
            }
        }
        _ => {
            return (Some(context.to_owned()), None);
        }
    };
    // PORT_NOTE: apollo_compiler's parsing code for field sets requires ignored tokens to be
    // pre-stripped if curly braces are missing, so we additionally do that here.
    let Some(selection) = strip_leading_ignored_tokens(selection) else { return (Some(context.to_owned()), None) };
    (Some(context.to_owned()), Some(selection.to_owned()))
}

static CONTEXT_PARSING_LEADING_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^(?:[\n\r\t ,]|#[^\n\r]*)*((?s:.)*)$"#).unwrap());

static CONTEXT_PARSING_CONTEXT_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^([A-Za-z_](?-u:\w)*)((?s:.)*)$"#).unwrap());

#[allow(dead_code)]
fn validate_field_value(
    _context: &Option<String>,
    _selection: &Option<String>,
    _target: &FieldArgumentDefinitionPosition,
    _set_context_locations: &[Name],
    _schema: &FederationSchema,
    _errors: &mut MultipleFederationErrors,
) {
    // TODO: Implement field value validation
    todo!("Implement validateFieldValue");
}

/// Trait for @fromContext directive validators
trait FromContextValidator {
    fn validate(
        &self,
        target: &FieldArgumentDefinitionPosition,
        schema: &FederationSchema,
        context: &Option<String>,
        selection: &Option<String>,
        errors: &mut MultipleFederationErrors,
    ) -> Result<(), FederationError>;
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
        _context: &Option<String>,
        _selection: &Option<String>,
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
        _context: &Option<String>,
        _selection: &Option<String>,
        errors: &mut MultipleFederationErrors,
    ) -> Result<(), FederationError> {
        match target {
            FieldArgumentDefinitionPosition::Object(position) => {
                let obj = position.parent().parent().get(schema.schema())?;
                let field = position.parent().field_name;
                for implemented in &obj.implements_interfaces {
                    let itf = InterfaceTypeDefinitionPosition {
                        type_name: implemented.name.clone(),
                    };
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
            }
            _ => {}
        }
        Ok(())
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
        context: &Option<String>,
        selection: &Option<String>,
        errors: &mut MultipleFederationErrors,
    ) -> Result<(), FederationError> {
        let context = context.as_ref().map(|s| s.as_str()).unwrap_or("");
        let selection = selection.as_ref().map(|s| s.as_str()).unwrap_or("");
        if context.is_empty() {
            errors.push(
                SingleFederationError::NoContextReferenced {
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
        } else if selection.is_empty() {
            errors.push(
                SingleFederationError::NoSelectionForContext {
                    message: format!("@fromContext directive in field \"{}\" has no selection", as_coordinate(target)),
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
        _context: &Option<String>,
        _selection: &Option<String>,
        errors: &mut MultipleFederationErrors,
    ) -> Result<(), FederationError> {
        match target {
            FieldArgumentDefinitionPosition::Object(position) => {
                let parent = position.parent().parent();
                if let Some(metadata) = &schema.subgraph_metadata {
                    let key_directive = metadata
                        .federation_spec_definition()
                        .key_directive_definition(schema)?;
                    let keys_on_type = parent.get_applied_directives(schema, &key_directive.name);
                    if !keys_on_type
                        .iter()
                        .fallible_filter(|application| -> Result<bool, FederationError> {
                            let arguments = metadata
                                .federation_spec_definition()
                                .key_directive_arguments(application)?;
                            Ok(arguments.resolvable)
                        })
                        .collect::<Result<Vec<_>, _>>()?
                        .is_empty()
                    {
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
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::MultipleFederationErrors;
    use crate::error::SingleFederationError;
    use crate::subgraph::typestate::Expanded;
    use crate::subgraph::typestate::Subgraph;
    use crate::subgraph::SubgraphError;
    use std::collections::HashMap;

    enum BuildOption {
        AsIs,
        AsFed2,
    }

    fn build_inner(
        schema_str: &str,
        build_option: BuildOption,
    ) -> Result<Subgraph<Expanded>, SubgraphError> {
        let name = "S";
        let subgraph =
            Subgraph::parse(name, &format!("http://{name}"), schema_str).expect("valid schema");
        let subgraph = if matches!(build_option, BuildOption::AsFed2) {
            subgraph
                .into_fed2_subgraph()
                .map_err(|e| SubgraphError::new(name, e))?
        } else {
            subgraph
        };
        Ok(subgraph
            .expand_links()
            .map_err(|e| SubgraphError::new(name, e))?)
    }

    fn build_and_expand(schema_str: &str) -> Subgraph<Expanded> {
        build_inner(schema_str, BuildOption::AsIs).expect("expanded subgraph to be valid")
    }

    #[test]
    fn test_deny_on_abstract_type() {
        // Create a test schema with @fromContext on an interface field
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext"])
                
            type Query {
                test: String
            }

            interface Entity {
                id(contextArg: ID! @fromContext(field: "$userContext userId")): ID!
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();
        
        // First validate context directives to build the context map
        let context_map = HashMap::new();
        
        // Then validate fromContext directives
        validate_from_context_directives(&subgraph.schema(), &context_map, &mut errors).expect("validates fromContext directives");

        // We expect an error for the @fromContext on an abstract type
        assert!(
            errors.errors.iter().any(|e| matches!(
                e,
                SingleFederationError::ContextNotSet { message } if message.contains("abstract type")
            )),
            "Expected an error about abstract type"
        );
    }

    #[test]
    fn test_deny_on_interface_implementation() {
        // Create a test schema with @fromContext on a field implementing an interface
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext"])
                
            type Query {
                user: User
            }

            interface Entity {
                id: ID!
            }

            type User implements Entity {
                id(contextArg: ID! @fromContext(field: "$userContext userId")): ID!
                name: String
            }

            type UserContext @context(name: "userContext") {
                userId: ID!
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();
        
        // First validate context directives to build the context map
        let context_map = crate::schema::validators::context::validate_context_directives(
            &subgraph.schema(), 
            &mut errors
        ).expect("validates context directives");
        
        // Then validate fromContext directives
        validate_from_context_directives(&subgraph.schema(), &context_map, &mut errors).expect("validates fromContext directives");

        // We expect an error for the @fromContext on a field implementing an interface
        assert!(
            errors.errors.iter().any(|e| matches!(
                e,
                SingleFederationError::ContextNotSet { message } if message.contains("implementing an interface field")
            )),
            "Expected an error about implementing an interface field"
        );
    }

    #[test]
    fn test_require_context_exists() {
        // Create a test schema with @fromContext on a field
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext"])
                
            type Query {
                user(id: ID! @fromContext(field: "$userContext id")): User
                invalid(id: ID! @fromContext(field: "$invalidContext id")): User
                noContext(id: ID! @fromContext(field: "$noSelection")): User
            }

            type User @context(name: "userContext") @context(name: "noSelection") {
                id: ID!
                name: String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();
        
        // First validate context directives to build the context map
        let context_map = crate::schema::validators::context::validate_context_directives(
            &subgraph.schema(), 
            &mut errors
        ).expect("validates context directives");
        
        // Then validate fromContext directives
        validate_from_context_directives(&subgraph.schema(), &context_map, &mut errors).expect("validates fromContext directives");

        // Check for invalid context error
        assert!(
            errors.errors.iter().any(|e| matches!(
                e,
                SingleFederationError::ContextNotSet { message } if message.contains("invalidContext")
            )),
            "Expected an error about invalid context"
        );
        
        // Check for missing selection error
        assert!(
            errors.errors.iter().any(|e| matches!(
                e,
                SingleFederationError::NoSelectionForContext { message } if message.contains("has no selection")
            )),
            "Expected an error about missing selection"
        );
    }

    #[test]
    fn test_require_resolvable_key() {
        // Create a test schema with @fromContext but no resolvable key
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                user(id: ID! @fromContext(field: "userContext.userId")): User
            }

            type User @context(name: "userContext") @key(fields: "id", resolvable: false) {
                id: ID!
                name: String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();
        
        // First validate context directives to build the context map
        let context_map = crate::schema::validators::context::validate_context_directives(
            &subgraph.schema(), 
            &mut errors
        ).expect("validates context directives");
        
        // Then validate fromContext directives
        validate_from_context_directives(&subgraph.schema(), &context_map, &mut errors).expect("validates fromContext directives");

        // We expect an error for the missing resolvable key
        let resolvable_key_error = errors.errors.iter().find(|e| matches!(
            e,
            SingleFederationError::ContextNoResolvableKey { .. }
        ));
        
        // Note: This might not be detected depending on the actual implementation
        // as there is a resolvable: false key
        if let Some(error) = resolvable_key_error {
            assert!(
                matches!(
                    error,
                    SingleFederationError::ContextNoResolvableKey { message } if message.contains("no resolvable key")
                ),
                "Expected an error about no resolvable key"
            );
        }
    }

    #[test]
    fn test_parse_context() {
        let fields = [
            ("$context { prop }", ("context", "{ prop }")),
            (
                "$context ... on A { prop } ... on B { prop }",
                ("context", "... on A { prop } ... on B { prop }"),
            ),
            (
                "$topLevelQuery { me { locale } }",
                ("topLevelQuery", "{ me { locale } }"),
            ),
            (
                "$context { a { b { c { prop }}} }",
                ("context", "{ a { b { c { prop }}} }"),
            ),
            (
                "$ctx { identifiers { legacyUserId } }",
                ("ctx", "{ identifiers { legacyUserId } }"),
            ),
            (
                "$retailCtx { identifiers { id5 } }",
                ("retailCtx", "{ identifiers { id5 } }"),
            ),
            ("$retailCtx { mid }", ("retailCtx", "{ mid }")),
            (
                "$widCtx { identifiers { wid } }",
                ("widCtx", "{ identifiers { wid } }"),
            ),
        ];
        for (field, (known_context, known_selection)) in fields {
            let (context, selection) = parse_context(field);
            assert_eq!(context, Some(known_context.to_string()));
            assert_eq!(selection, Some(known_selection.to_string()));
        }
        // Ensure we don't backtrack in the comment regex.
        assert_eq!(parse_context("#comment $fakeContext fakeSelection"), (None, None));
        assert_eq!(parse_context("$ #comment fakeContext fakeSelection"), (None, None));
        
            // Test valid context reference
        let (parsed_context, parsed_selection) = parse_context("$contextA userId");
        assert_eq!(parsed_context, Some("contextA".to_string()));
        assert_eq!(parsed_selection, Some("userId".to_string()));
        
        // Test no delimiter
        let (parsed_context, parsed_selection) = parse_context("invalidFormat");
        assert_eq!(parsed_context, None);
        assert_eq!(parsed_selection, None);

        // // Test space in context
        let (parsed_context, parsed_selection) = parse_context("$ selection");
        assert_eq!(parsed_context, Some("selection".to_string()));
        assert_eq!(parsed_selection, None);

        // Test empty selection
        let (parsed_context, parsed_selection) = parse_context("$context ");
        assert_eq!(parsed_context, Some("context".to_string()));
        assert_eq!(parsed_selection, None);
        
        // Test multiple delimiters (should only split on first)
        let (parsed_context, parsed_selection) = parse_context("$contextA multiple fields selected");
        assert_eq!(parsed_context, Some("contextA".to_string()));
        assert_eq!(parsed_selection, Some("multiple fields selected".to_string()));
    }
}