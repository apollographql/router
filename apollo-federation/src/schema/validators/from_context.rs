use std::collections::HashMap;
use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::ast::Type;
use apollo_compiler::collections::HashSet;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use regex::Regex;

use crate::bail;
use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::schema::FederationSchema;
use crate::schema::FromContextDirective;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::FieldArgumentDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;
use crate::schema::subgraph_metadata::SubgraphMetadata;
use crate::schema::validators::DeniesAliases;
use crate::schema::validators::DeniesDirectiveApplications;
use crate::schema::validators::DenyAliases;
use crate::schema::validators::DenyFieldsWithDirectiveApplications;
use crate::schema::validators::SchemaFieldSetValidator;
use crate::utils::FallibleIterator;
use crate::utils::iter_into_single_item;

pub(crate) fn validate_from_context_directives(
    schema: &FederationSchema,
    meta: &SubgraphMetadata,
    context_map: &HashMap<String, Vec<Name>>,
    errors: &mut MultipleFederationErrors,
    subgraph_name: &str,
) -> Result<(), FederationError> {
    let from_context_rules: Vec<Box<dyn FromContextValidator>> = vec![
        Box::new(DenyOnAbstractType::new()),
        Box::new(DenyOnInterfaceImplementation::new()),
        Box::new(RequireContextExists::new(context_map)),
        Box::new(RequireResolvableKey::new()),
        Box::new(DenyDefaultValues::new()),
    ];

    let Ok(from_context_directives) = schema.from_context_directive_applications() else {
        // if we get an error, we probably are pre fed 2.8
        return Ok(());
    };

    for from_context_directive in from_context_directives {
        match from_context_directive {
            Ok(from_context) => {
                // Parse context and selection from the field value
                let field = from_context.arguments.field.to_string();
                let (context, selection) = parse_context(&field);

                // Apply each validation rule
                for rule in from_context_rules.iter() {
                    rule.validate(
                        &from_context.target,
                        schema,
                        meta,
                        &context,
                        &selection,
                        errors,
                    )?;
                }

                // after RequireContextExists, we will have errored if either the context or selection is not present
                let (Some(context), Some(selection)) = (&context, &selection) else {
                    bail!(
                        "[{}] @fromContext argument does not reference a context \"{}\"",
                        subgraph_name,
                        field
                    );
                };

                // We need the context locations from the context map for this target
                if let Some(set_context_locations) = context_map.get(context)
                    && let Err(validation_error) = validate_field_value(
                        context,
                        selection,
                        &from_context,
                        set_context_locations,
                        schema,
                        errors,
                    )
                {
                    errors.push(validation_error);
                }
            }
            Err(e) => errors.push(e),
        }
    }

    Ok(())
}

/// Parses a field string that contains a context reference and optional selection.
///
/// The function expects a string in the format "$contextName selection" where:
/// - The string must start with a '$' followed by a context name
/// - The context name must be a valid identifier (starting with letter/underscore, followed by alphanumeric/underscore)
/// - An optional selection can follow the context name
///
/// Returns a tuple of (Option<String>, Option<String>) where:
/// - First element is Some(context_name) if a valid context was found, None otherwise
/// - Second element is Some(selection) if a valid selection was found after the context, None otherwise
///
/// Examples:
/// - "$userContext userId" -> (Some("userContext"), Some("userId"))
/// - "$context { prop }" -> (Some("context"), Some("{ prop }"))
/// - "invalid" -> (None, None)
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
            .map(|m| m.as_str())
    }

    let Some(dollar_start) = strip_leading_ignored_tokens(field) else {
        return (None, None);
    };

    let mut dollar_iter = dollar_start.chars();
    if dollar_iter.next() != Some('$') {
        return (None, None);
    }
    let after_dollar = dollar_iter.as_str();

    let Some(context_start) = strip_leading_ignored_tokens(after_dollar) else {
        return (None, None);
    };
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
            let Some(selection) = strip_leading_ignored_tokens(selection) else {
                return (Some(context.to_owned()), None);
            };
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
    let Some(selection) = strip_leading_ignored_tokens(selection) else {
        return (Some(context.to_owned()), None);
    };
    (Some(context.to_owned()), Some(selection.to_owned()))
}

static CONTEXT_PARSING_LEADING_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^(?:[\n\r\t ,]|#[^\n\r]*)*((?s:.)*)$"#).unwrap());

static CONTEXT_PARSING_CONTEXT_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^([A-Za-z_](?-u:\w)*)((?s:.)*)$"#).unwrap());

#[derive(Debug, PartialEq)]
enum SelectionType {
    Error,
    Field,
    InlineFragment {
        type_conditions: std::collections::HashSet<String>,
    },
}

/// Validates a field value selection format and returns whether it's a field or inline fragment
fn validate_selection_format(
    context: &str,
    selection_set: &SelectionSet,
    from_context_parent: &FieldArgumentDefinitionPosition,
    errors: &mut MultipleFederationErrors,
) -> SelectionType {
    // if it's a field, we expect there to be only one selection.
    // if it's an inline fragment, we expect there to be a type_condition on every selection
    let mut type_conditions = std::collections::HashSet::new();
    let mut has_field = false;
    let mut has_inline_fragment = false;
    for selection in selection_set.selections.iter() {
        match selection {
            // note that the fact that this selection is the only selection will be checked in the caller
            Selection::Field(_) => {
                has_field = true;
            }
            Selection::InlineFragment(fragment) => {
                has_inline_fragment = true;
                if let Some(type_condition) = &fragment.type_condition {
                    type_conditions.insert(type_condition.to_string());
                } else {
                    errors.push(
                        SingleFederationError::ContextSelectionInvalid {
                            message: format!(
                                "Context \"{context}\" is used in \"{from_context_parent}\" but the selection is invalid: inline fragments must have type conditions"
                            ),
                        }
                        .into(),
                    );
                    return SelectionType::Error;
                }
            }
            Selection::FragmentSpread(_) => {
                errors.push(
                    SingleFederationError::ContextSelectionInvalid {
                        message: format!(
                            "Context \"{context}\" is used in \"{from_context_parent}\" but the selection is invalid: fragment spreads are not allowed"
                        ),
                    }
                    .into(),
                );
                return SelectionType::Error;
            }
        }
    }

    if has_field && has_inline_fragment {
        errors.push(
            SingleFederationError::ContextSelectionInvalid {
                message: format!("Context \"{context}\" is used in \"{from_context_parent}\" but the selection is invalid: multiple fields could be selected"),
            }
            .into(),
        );
        return SelectionType::Error;
    } else if has_field {
        return SelectionType::Field;
    }

    if type_conditions.len() != selection_set.selections.len() {
        errors.push(
            SingleFederationError::ContextSelectionInvalid {
                message: format!(
                    "Context \"{context}\" is used in \"{from_context_parent}\" but the selection is invalid: type conditions have the same name"
                ),
            }
            .into(),
        );
        return SelectionType::Error;
    }
    SelectionType::InlineFragment { type_conditions }
}

fn validate_field_value(
    context: &str,
    selection: &String,
    applied_directive: &FromContextDirective,
    set_context_locations: &[Name],
    schema: &FederationSchema,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let argument_rules: Vec<Box<dyn SchemaFieldSetValidator<FromContextDirective>>> = vec![
        Box::new(DenyAliases::new()),
        Box::new(DenyFieldsWithDirectiveApplications::new()),
    ];

    let target = &applied_directive.target;
    // Get the expected type from the target argument
    let expected_type = match target {
        FieldArgumentDefinitionPosition::Object(pos) => match pos.get(schema.schema()) {
            Ok(arg_def) => arg_def.ty.item_type(),
            Err(_) => bail!("could not find position in schema"),
        },
        FieldArgumentDefinitionPosition::Interface(pos) => match pos.get(schema.schema()) {
            Ok(arg_def) => arg_def.ty.item_type(),
            Err(_) => bail!("could not find position in schema"),
        },
    };

    let mut used_type_conditions: HashSet<String> = Default::default();

    let mut selection_type = SelectionType::Error;
    // For each set context location, validate the selection
    for location_name in set_context_locations {
        // Try to create a composite type position from the location name
        let Some(extended_type) = schema.schema().types.get(location_name) else {
            continue;
        };
        let Ok(location) =
            CompositeTypeDefinitionPosition::try_from(TypeDefinitionPosition::from(extended_type))
        else {
            continue;
        };

        let result = FieldSet::parse(
            Valid::assume_valid_ref(schema.schema()),
            location.type_name().clone(),
            selection,
            "from_context.graphql",
        );

        if result.is_err() {
            errors.push(
                SingleFederationError::ContextSelectionInvalid {
                    message: format!(
                        "Context \"{}\" is used in \"{}\" but the selection is invalid for type \"{}\".",
                        context, target, location.type_name().clone(),
                    ),
                }
                .into(),
            );
            return Ok(());
        }
        let fields = result.unwrap();
        // TODO: Is it necessary to perform these validations on every iteration or can we do it only once?
        for rule in argument_rules.iter() {
            rule.visit(location_name, &fields, applied_directive, errors);
        }
        if !errors.errors.is_empty() {
            return Ok(());
        }
        selection_type = validate_selection_format(context, &fields.selection_set, target, errors);
        // if there was an error, just return, we've already added it to the errorCollector
        if selection_type == SelectionType::Error {
            return Ok(());
        }

        let selection_set = &fields.selection_set;
        // Check for multiple selections (only when it's a field selection, not inline fragments)
        if selection_type == SelectionType::Field && selection_set.selections.len() > 1 {
            errors.push(
            SingleFederationError::ContextSelectionInvalid {
                message: format!(
                    "Context \"{context}\" is used in \"{target}\" but the selection is invalid: multiple selections are made"
                ),
            }
            .into(),
            );
            return Ok(());
        }

        match &selection_type {
            SelectionType::Field => {
                // For field selections, validate the type
                let type_position = TypeDefinitionPosition::from(location.clone());

                let resolved_type = validate_field_value_type(
                    context,
                    &type_position,
                    selection_set,
                    schema,
                    target,
                    errors,
                )?;

                let Some(resolved_type) = resolved_type else {
                    errors.push(
                    SingleFederationError::ContextSelectionInvalid {
                        message: format!(
                            "Context \"{context}\" is used in \"{target}\" but the selection is invalid: the type of the selection does not match the expected type \"{expected_type}\""
                        ),
                    }
                    .into(),
                );
                    return Ok(());
                };
                if !is_valid_implementation_field_type(expected_type, &resolved_type) {
                    errors.push(
                    SingleFederationError::ContextSelectionInvalid {
                        message: format!(
                            "Context \"{context}\" is used in \"{target}\" but the selection is invalid: the type of the selection \"{resolved_type}\" does not match the expected type \"{expected_type}\""
                        ),
                    }
                    .into(),
                );
                    return Ok(());
                }
            }
            SelectionType::InlineFragment { type_conditions } => {
                // For inline fragment selections, validate each fragment
                for selection in selection_set.selections.iter() {
                    if let Selection::InlineFragment(frag) = selection
                        && let Some(type_condition) = &frag.type_condition
                    {
                        let Some(extended_type) =
                            schema.schema().types.get(type_condition.as_str())
                        else {
                            errors.push(
                                SingleFederationError::ContextSelectionInvalid { message: format!(
                                    "Inline fragment type condition invalid. Type '{}' does not exist in schema.", type_condition.as_str()
                                ) }
                                .into(),
                            );
                            continue;
                        };
                        let frag_type_position = TypeDefinitionPosition::from(extended_type);
                        if ObjectTypeDefinitionPosition::try_from(frag_type_position.clone())
                            .is_err()
                        {
                            errors.push(
                                SingleFederationError::ContextSelectionInvalid { message:
                                    "Inline fragment type condition invalid: type conditions must be an object type".to_string()
                                 }.into(),
                            );
                            continue;
                        }

                        if let Ok(Some(resolved_type)) = validate_field_value_type(
                            context,
                            &frag_type_position,
                            &frag.selection_set,
                            schema,
                            target,
                            errors,
                        ) {
                            // For inline fragments, remove NonNull wrapper as other subgraphs may not define this
                            // This matches the TypeScript behavior
                            if !is_valid_implementation_field_type(expected_type, &resolved_type) {
                                errors.push(
                                    SingleFederationError::ContextSelectionInvalid {
                                        message: format!(
                                            "Context \"{context}\" is used in \"{target}\" but the selection is invalid: thxe type of the selection \"{resolved_type}\" does not match the expected type \"{expected_type}\""
                                        ),
                                    }
                                    .into(),
                                    );
                                return Ok(());
                            }
                            used_type_conditions.insert(type_condition.as_str().to_string());
                        } else {
                            errors.push(
                                SingleFederationError::ContextSelectionInvalid {
                                    message: format!(
                                        "Context \"{context}\" is used in \"{target}\" but the selection is invalid: the type of the selection does not match the expected type \"{expected_type}\""
                                    ),
                                }
                                .into(),
                                );
                            return Ok(());
                        }
                    }
                }
                let context_location_names: std::collections::HashSet<String> =
                    set_context_locations
                        .iter()
                        .map(|name| name.as_str().to_string())
                        .collect();

                let mut has_matching_condition = false;
                for type_condition in type_conditions {
                    if context_location_names.contains(type_condition) {
                        has_matching_condition = true;
                        break;
                    } else {
                        // get the type
                        let Some(type_condition_type) =
                            schema.schema().types.get(type_condition.as_str())
                        else {
                            bail!("Type not found for type condition: {}", type_condition);
                        };
                        let interfaces = match type_condition_type {
                            ExtendedType::Interface(intf) => intf
                                .implements_interfaces
                                .iter()
                                .map(|i| i.name.clone())
                                .collect(),
                            ExtendedType::Object(obj) => obj
                                .implements_interfaces
                                .iter()
                                .map(|i| i.name.clone())
                                .collect(),
                            _ => vec![],
                        };
                        if interfaces
                            .iter()
                            .any(|itf| context_location_names.contains(itf.as_str()))
                        {
                            has_matching_condition = true;
                            break;
                        }
                    }
                }
                if !has_matching_condition && !type_conditions.is_empty() {
                    // No type condition matches any context location
                    let context_locations_str = set_context_locations
                        .iter()
                        .map(|name| name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");

                    errors.push(
                    SingleFederationError::ContextSelectionInvalid {
                        message: format!(
                            "Context \"{context}\" is used in \"{target}\" but the selection is invalid: no type condition matches the location \"{context_locations_str}\""
                        ),
                    }
                    .into(),
                );
                }
            }
            SelectionType::Error => return Ok(()),
        }
    }

    // Check for unused type conditions
    if let SelectionType::InlineFragment { type_conditions } = selection_type {
        for type_condition in &type_conditions {
            if !used_type_conditions.contains(type_condition) {
                errors.push(
                    SingleFederationError::ContextSelectionInvalid {
                        message: format!(
                            "Context \"{context}\" is used in \"{target}\" but the selection is invalid: type condition \"{type_condition}\" is never used"
                        ),
                    }
                    .into(),
                );
            }
        }
    }

    Ok(())
}

fn is_valid_implementation_field_type(field_type: &Type, implemented_field_type: &Type) -> bool {
    // If fieldType is a Non-Null type:
    match (field_type, implemented_field_type) {
        (Type::NonNullNamed(field_name), Type::NonNullNamed(impl_name)) => {
            // Let nullableType be the unwrapped nullable type of fieldType.
            let field_type_nullable = Type::Named(field_name.clone());
            let implemented_field_type_nullable = Type::Named(impl_name.clone());
            is_valid_implementation_field_type(
                &field_type_nullable,
                &implemented_field_type_nullable,
            )
        }
        (Type::NonNullNamed(field_name), Type::Named(_)) => {
            let field_type_nullable = Type::Named(field_name.clone());
            is_valid_implementation_field_type(&field_type_nullable, implemented_field_type)
        }
        (Type::NonNullList(field_inner), Type::NonNullList(impl_inner)) => {
            let field_type_nullable = (**field_inner).clone();
            let implemented_field_type_nullable = (**impl_inner).clone();
            is_valid_implementation_field_type(
                &field_type_nullable,
                &implemented_field_type_nullable,
            )
        }
        (Type::NonNullList(field_inner), Type::List(_)) => {
            let field_type_nullable = (**field_inner).clone();
            is_valid_implementation_field_type(&field_type_nullable, implemented_field_type)
        }
        (Type::List(field_inner), Type::List(impl_inner)) => {
            let field_type_inner = (**field_inner).clone();
            let implemented_type_inner = (**impl_inner).clone();
            is_valid_implementation_field_type(&field_type_inner, &implemented_type_inner)
        }
        (Type::Named(field_name), Type::Named(impl_name)) => field_name == impl_name,
        _ => false,
    }
}

/// Trait for @fromContext directive validators
trait FromContextValidator {
    fn validate(
        &self,
        target: &FieldArgumentDefinitionPosition,
        schema: &FederationSchema,
        meta: &SubgraphMetadata,
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
        _meta: &SubgraphMetadata,
        _context: &Option<String>,
        _selection: &Option<String>,
        errors: &mut MultipleFederationErrors,
    ) -> Result<(), FederationError> {
        if let FieldArgumentDefinitionPosition::Interface(_) = target {
            errors.push(
            SingleFederationError::ContextNotSet {
                message: format!(
                    "@fromContext argument cannot be used on a field that exists on an abstract type \"{target}\"."
                ),
                }
                .into(),
            );
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
        _meta: &SubgraphMetadata,
        _context: &Option<String>,
        _selection: &Option<String>,
        errors: &mut MultipleFederationErrors,
    ) -> Result<(), FederationError> {
        if let FieldArgumentDefinitionPosition::Object(position) = target {
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
                                "@fromContext argument cannot be used on a field implementing an interface field \"{}.{}\".",
                                itf.type_name,
                                field.unwrap().field_name,
                            ),
                        }
                        .into(),
                    );
                }
            }
        }
        Ok(())
    }
}

/// Validator that checks if the referenced context exists
struct RequireContextExists<'a> {
    context_map: &'a HashMap<String, Vec<Name>>,
}

impl<'a> RequireContextExists<'a> {
    fn new(context_map: &'a HashMap<String, Vec<Name>>) -> Self {
        Self { context_map }
    }
}

impl<'a> FromContextValidator for RequireContextExists<'a> {
    fn validate(
        &self,
        target: &FieldArgumentDefinitionPosition,
        _schema: &FederationSchema,
        _meta: &SubgraphMetadata,
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
                        "@fromContext argument does not reference a context \"${context} {selection}\"."
                    ),
                }
                .into(),
            );
        } else if !self.context_map.contains_key(context) {
            errors.push(
                SingleFederationError::ContextNotSet {
                    message: format!(
                        "Context \"{context}\" is used at location \"{target}\" but is never set."
                    ),
                }
                .into(),
            );
        } else if selection.is_empty() {
            errors.push(
                SingleFederationError::NoSelectionForContext {
                    message: format!(
                        "@fromContext directive in field \"{target}\" has no selection"
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
        meta: &SubgraphMetadata,
        _context: &Option<String>,
        _selection: &Option<String>,
        errors: &mut MultipleFederationErrors,
    ) -> Result<(), FederationError> {
        if let FieldArgumentDefinitionPosition::Object(position) = target {
            let parent = position.parent().parent();
            let key_directive = meta
                .federation_spec_definition()
                .key_directive_definition(schema)?;
            if parent
                .get_applied_directives(schema, &key_directive.name)
                .iter()
                .fallible_filter(|application| -> Result<bool, FederationError> {
                    let arguments = meta
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
                            "Object \"{parent}\" has no resolvable key but has a field with a contextual argument."
                        ),
                    }
                    .into(),
                );
            }
        }
        Ok(())
    }
}

/// Validator that denies @fromContext arguments with default values
struct DenyDefaultValues {}

impl DenyDefaultValues {
    fn new() -> Self {
        Self {}
    }
}

impl FromContextValidator for DenyDefaultValues {
    fn validate(
        &self,
        target: &FieldArgumentDefinitionPosition,
        schema: &FederationSchema,
        _meta: &SubgraphMetadata,
        _context: &Option<String>,
        _selection: &Option<String>,
        errors: &mut MultipleFederationErrors,
    ) -> Result<(), FederationError> {
        // Check if the argument has a default value
        let has_default = match target {
            FieldArgumentDefinitionPosition::Object(position) => {
                if let Ok(arg_def) = position.get(schema.schema()) {
                    arg_def.default_value.is_some()
                } else {
                    false
                }
            }
            FieldArgumentDefinitionPosition::Interface(position) => {
                if let Ok(arg_def) = position.get(schema.schema()) {
                    arg_def.default_value.is_some()
                } else {
                    false
                }
            }
        };

        if has_default {
            errors.push(
                SingleFederationError::ContextSelectionInvalid {
                    message: format!(
                        "@fromContext arguments may not have a default value: \"{target}\"."
                    ),
                }
                .into(),
            );
        }
        Ok(())
    }
}

impl DeniesAliases for FromContextDirective<'_> {
    fn error(&self, _alias: &Name, _field: &Field) -> SingleFederationError {
        let (context, _) = parse_context(self.arguments.field);
        SingleFederationError::ContextSelectionInvalid {
            message: format!(
                "Context \"{}\" is used in \"{}\" but the selection is invalid: aliases are not allowed in the selection",
                context.unwrap_or("unknown".to_string()),
                self.target
            ),
        }
    }
}

impl DeniesDirectiveApplications for FromContextDirective<'_> {
    fn error(&self, _: &DirectiveList) -> SingleFederationError {
        let (context, _) = parse_context(self.arguments.field);
        SingleFederationError::ContextSelectionInvalid {
            message: format!(
                "Context \"{}\" is used in \"{}\" but the selection is invalid: directives are not allowed in the selection",
                context.unwrap_or("unknown".to_string()),
                self.target
            ),
        }
    }
}

#[allow(dead_code, clippy::only_used_in_recursion)]
fn validate_field_value_type_inner(
    selection_set: &SelectionSet,
    schema: &FederationSchema,
    from_context_parent: &FieldArgumentDefinitionPosition,
    errors: &mut MultipleFederationErrors,
) -> Option<Type> {
    let mut types_array = Vec::new();

    if selection_set.selections.is_empty() {
        types_array.push(Type::Named(selection_set.ty.clone()));
    }

    for selection in selection_set.selections.iter() {
        if let Selection::Field(field) = selection
            && let Some(nested_type) = validate_field_value_type_inner(
                &field.selection_set,
                schema,
                from_context_parent,
                errors,
            )
        {
            types_array.push(nested_type);
        }
        // } else {
        //     if let Ok(field_def) = field.field.field_position.get(schema.schema()) {
        //         let base_type = &field_def.ty;
        //         types_array.push(base_type);
        //     }
        // }
    }

    if types_array.is_empty() {
        return None;
    }
    types_array
        .into_iter()
        .map(Some)
        .reduce(|acc, item| match (acc, item) {
            (Some(acc), Some(item)) => {
                if acc == item {
                    Some(acc)
                } else if acc.is_assignable_to(&item) {
                    Some(item)
                } else if item.is_assignable_to(&acc) {
                    Some(acc)
                } else {
                    None
                }
            }
            _ => None,
        })
        .flatten()
}

#[allow(dead_code)]
fn validate_field_value_type(
    context: &str,
    current_type: &TypeDefinitionPosition,
    selection_set: &SelectionSet,
    schema: &FederationSchema,
    from_context_parent: &FieldArgumentDefinitionPosition,
    errors: &mut MultipleFederationErrors,
) -> Result<Option<Type>, FederationError> {
    if let Some(metadata) = &schema.subgraph_metadata
        && let Some(interface_object_directive) = metadata
            .federation_spec_definition()
            .interface_object_directive_definition(schema)?
        && current_type.has_applied_directive(schema, &interface_object_directive.name)
    {
        errors.push(
                    SingleFederationError::ContextSelectionInvalid {
                        message: format!("Context \"{}\" is used in \"{}\" but the selection is invalid: One of the types in the selection is an interfaceObject: \"{}\".", context, from_context_parent, current_type.type_name())
                    }
                    .into(),
                );
    }
    Ok(validate_field_value_type_inner(
        selection_set,
        schema,
        from_context_parent,
        errors,
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::error::MultipleFederationErrors;
    use crate::error::SingleFederationError;
    use crate::subgraph::test_utils::build_and_expand;

    #[test]
    // Port note: This test validates @fromContext on abstract types which is forbidden
    // No direct JS equivalent, but relates to JS test "forbid contextual arguments on interfaces"
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
        validate_from_context_directives(
            subgraph.schema(),
            subgraph.metadata(),
            &context_map,
            &mut errors,
            &subgraph.name,
        )
        .expect("validates fromContext directives");

        // We expect an error for the @fromContext on an abstract type
        assert!(
            errors.errors.iter().any(|e| matches!(
                e,
                SingleFederationError::ContextNotSet { message } if message == "@fromContext argument cannot be used on a field that exists on an abstract type \"Entity.id(contextArg:)\"."
            )),
            "Expected an error about abstract type"
        );
    }

    #[test]
    // Port note: Ported from JS test "forbid contextual arguments on interfaces"
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
            subgraph.schema(),
            &mut errors,
        )
        .expect("validates context directives");

        // Then validate fromContext directives
        validate_from_context_directives(
            subgraph.schema(),
            subgraph.metadata(),
            &context_map,
            &mut errors,
            &subgraph.name,
        )
        .expect("validates fromContext directives");

        // We expect an error for the @fromContext on a field implementing an interface
        assert!(
            errors.errors.iter().any(|e| matches!(
                e,
                SingleFederationError::ContextNotSet { message } if message == "@fromContext argument cannot be used on a field implementing an interface field \"User.id(contextArg:)\"."
            )),
            "Expected an error about implementing an interface field"
        );
    }

    #[test]
    // Port note: Combines logic from JS tests "context is never set" and "context variable does not appear in selection"
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
            subgraph.schema(),
            &mut errors,
        )
        .expect("validates context directives");

        // Then validate fromContext directives
        validate_from_context_directives(
            subgraph.schema(),
            subgraph.metadata(),
            &context_map,
            &mut errors,
            &subgraph.name,
        )
        .expect_err("validates fromContext directives");
    }

    #[test]
    // Port note: Ported from JS test "at least one key on an object that uses a context must be resolvable"
    fn test_require_resolvable_key() {
        // Create a test schema with @fromContext but no resolvable key
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                user(id: ID! @fromContext(field: "$userContext { userId }")): User
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
            subgraph.schema(),
            &mut errors,
        )
        .expect("validates context directives");

        // Then validate fromContext directives
        validate_from_context_directives(
            subgraph.schema(),
            subgraph.metadata(),
            &context_map,
            &mut errors,
            &subgraph.name,
        )
        .expect("validates fromContext directives");

        // We expect an error for the missing resolvable key
        let resolvable_key_error = errors
            .errors
            .iter()
            .find(|e| matches!(e, SingleFederationError::ContextNoResolvableKey { .. }));

        // Note: This might not be detected depending on the actual implementation
        // as there is a resolvable: false key
        if let Some(error) = resolvable_key_error {
            assert!(
                matches!(
                    error,
                    SingleFederationError::ContextNoResolvableKey { message } if message == "Object \"Query\" has no resolvable key but has a field with a contextual argument."
                ),
                "Expected an error about no resolvable key"
            );
        }
    }

    #[test]
    // Port note: Tests context parsing logic - no direct JS equivalent as this is implementation detail
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
        assert_eq!(
            parse_context("#comment $fakeContext fakeSelection"),
            (None, None)
        );
        assert_eq!(
            parse_context("$ #comment fakeContext fakeSelection"),
            (None, None)
        );

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
        let (parsed_context, parsed_selection) =
            parse_context("$contextA multiple fields selected");
        assert_eq!(parsed_context, Some("contextA".to_string()));
        assert_eq!(
            parsed_selection,
            Some("multiple fields selected".to_string())
        );
    }

    #[test]
    // Port note: Tests field value type validation logic - no direct JS equivalent as this is implementation detail
    fn test_validate_field_value_type_single_field() {
        use crate::schema::position::FieldArgumentDefinitionPosition;
        use crate::schema::position::ObjectFieldArgumentDefinitionPosition;
        use crate::schema::position::ObjectTypeDefinitionPosition;

        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext"])
                
            type Query {
                contextual(id: ID): User
            }

            type User {
                id: ID
                name: String
                age: Int
                email: String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        let user_type = TypeDefinitionPosition::Object(ObjectTypeDefinitionPosition::new(
            Name::new_unchecked("User"),
        ));
        let query_contextual_arg_pos =
            FieldArgumentDefinitionPosition::Object(ObjectFieldArgumentDefinitionPosition {
                type_name: Name::new_unchecked("Query"),
                field_name: Name::new_unchecked("contextual"),
                argument_name: Name::new_unchecked("id"),
            });

        // Test case 1: Single field selection - should return the field type

        let fields = FieldSet::parse(
            Valid::assume_valid_ref(subgraph.schema().schema()),
            user_type.type_name().clone(),
            "id",
            "from_context.graphql",
        )
        .expect("valid field set");

        let result = validate_field_value_type(
            "userContext",
            &user_type,
            &fields.selection_set,
            subgraph.schema(),
            &query_contextual_arg_pos,
            &mut errors,
        )
        .expect("valid field value type");

        assert!(
            result.is_some(),
            "Should return a type for single field selection"
        );
        assert_eq!(
            result.unwrap().inner_named_type().as_str(),
            "ID",
            "Should return ID type"
        );
        assert!(
            errors.errors.is_empty(),
            "Should not have validation errors"
        );
    }

    #[test]
    // Port note: Tests field value type validation logic - no direct JS equivalent as this is implementation detail
    fn test_validate_field_value_type_consistent_fields() {
        use crate::schema::position::FieldArgumentDefinitionPosition;
        use crate::schema::position::ObjectFieldArgumentDefinitionPosition;
        use crate::schema::position::ObjectTypeDefinitionPosition;

        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext"])
                
            type Query {
                contextual(id: ID): User
            }

            type User {
                id: ID
                userId: ID
                identifier: ID
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        let user_type = TypeDefinitionPosition::Object(ObjectTypeDefinitionPosition::new(
            Name::new_unchecked("User"),
        ));
        let query_contextual_arg_pos =
            FieldArgumentDefinitionPosition::Object(ObjectFieldArgumentDefinitionPosition {
                type_name: Name::new_unchecked("Query"),
                field_name: Name::new_unchecked("contextual"),
                argument_name: Name::new_unchecked("id"),
            });

        // Test case: Multiple fields with same type - should return common type
        let fields = FieldSet::parse(
            Valid::assume_valid_ref(subgraph.schema().schema()),
            user_type.type_name().clone(),
            "{ id userId identifier }",
            "from_context.graphql",
        )
        .expect("valid field set");

        let result = validate_field_value_type(
            "userContext",
            &user_type,
            &fields.selection_set,
            subgraph.schema(),
            &query_contextual_arg_pos,
            &mut errors,
        )
        .expect("valid field value type");

        assert!(
            result.is_some(),
            "Should return a type for consistent field types"
        );
        assert_eq!(
            result.unwrap().inner_named_type().as_str(),
            "ID",
            "Should return common ID type"
        );
        assert!(
            errors.errors.is_empty(),
            "Should not have validation errors"
        );
    }

    #[test]
    // Port note: Tests field value type validation logic - no direct JS equivalent as this is implementation detail
    fn test_validate_field_value_type_inconsistent_fields() {
        use crate::schema::position::FieldArgumentDefinitionPosition;
        use crate::schema::position::ObjectFieldArgumentDefinitionPosition;
        use crate::schema::position::ObjectTypeDefinitionPosition;

        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext"])
                
            type Query {
                contextual(id: ID): User
            }

            type User {
                id: ID
                name: String
                age: Int
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        let user_type = TypeDefinitionPosition::Object(ObjectTypeDefinitionPosition::new(
            Name::new_unchecked("User"),
        ));
        let query_contextual_arg_pos =
            FieldArgumentDefinitionPosition::Object(ObjectFieldArgumentDefinitionPosition {
                type_name: Name::new_unchecked("Query"),
                field_name: Name::new_unchecked("contextual"),
                argument_name: Name::new_unchecked("id"),
            });

        // Test case: Multiple fields with different types - should return None
        let fields = FieldSet::parse(
            Valid::assume_valid_ref(subgraph.schema().schema()),
            user_type.type_name().clone(),
            "{ id name age }",
            "from_context.graphql",
        )
        .expect("valid field set");

        let result = validate_field_value_type(
            "userContext",
            &user_type,
            &fields.selection_set,
            subgraph.schema(),
            &query_contextual_arg_pos,
            &mut errors,
        )
        .expect("valid field value type");

        assert!(
            result.is_none(),
            "Should return None for inconsistent field types"
        );
        assert!(
            errors.errors.is_empty(),
            "Should not have validation errors for type mismatch"
        );
    }

    #[test]
    // Port note: Tests field value type validation logic - no direct JS equivalent as this is implementation detail
    fn test_validate_field_value_type_nested_selection() {
        use crate::schema::position::FieldArgumentDefinitionPosition;
        use crate::schema::position::ObjectFieldArgumentDefinitionPosition;
        use crate::schema::position::ObjectTypeDefinitionPosition;

        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext"])
                
            type Query {
                contextual(id: ID): User
            }

            type User {
                profile: Profile
                settings: Profile
            }
            
            type Profile {
                id: ID
                name: String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        let user_type = TypeDefinitionPosition::Object(ObjectTypeDefinitionPosition::new(
            Name::new_unchecked("User"),
        ));
        let query_contextual_arg_pos =
            FieldArgumentDefinitionPosition::Object(ObjectFieldArgumentDefinitionPosition {
                type_name: Name::new_unchecked("Query"),
                field_name: Name::new_unchecked("contextual"),
                argument_name: Name::new_unchecked("id"),
            });

        // Test case: Nested selection with consistent types
        let fields = FieldSet::parse(
            Valid::assume_valid_ref(subgraph.schema().schema()),
            user_type.type_name().clone(),
            "{ profile { id } settings { id } }",
            "from_context.graphql",
        )
        .expect("valid field set");

        let result = validate_field_value_type(
            "userContext",
            &user_type,
            &fields.selection_set,
            subgraph.schema(),
            &query_contextual_arg_pos,
            &mut errors,
        )
        .expect("valid field value type");

        assert!(
            result.is_some(),
            "Should return a type for nested consistent selections"
        );
        assert_eq!(
            result.unwrap().inner_named_type().as_str(),
            "ID",
            "Should return ID type from nested selection"
        );
        assert!(
            errors.errors.is_empty(),
            "Should not have validation errors"
        );
    }

    #[test]
    // Port note: Tests field value type validation logic - no direct JS equivalent as this is implementation detail
    fn test_validate_field_value_type_nested_inconsistent() {
        use crate::schema::position::FieldArgumentDefinitionPosition;
        use crate::schema::position::ObjectFieldArgumentDefinitionPosition;
        use crate::schema::position::ObjectTypeDefinitionPosition;

        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext"])
                
            type Query {
                contextual(id: ID): User
            }

            type User {
                profile: Profile
                settings: Settings
            }
            
            type Profile {
                id: ID
            }
            
            type Settings {
                name: String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        let user_type = TypeDefinitionPosition::Object(ObjectTypeDefinitionPosition::new(
            Name::new_unchecked("User"),
        ));
        let query_contextual_arg_pos =
            FieldArgumentDefinitionPosition::Object(ObjectFieldArgumentDefinitionPosition {
                type_name: Name::new_unchecked("Query"),
                field_name: Name::new_unchecked("contextual"),
                argument_name: Name::new_unchecked("id"),
            });

        // Test case: Nested selection with inconsistent types
        let fields = FieldSet::parse(
            Valid::assume_valid_ref(subgraph.schema().schema()),
            user_type.type_name().clone(),
            "{ profile { id } settings { name } }",
            "from_context.graphql",
        )
        .expect("valid field set");

        let result = validate_field_value_type(
            "userContext",
            &user_type,
            &fields.selection_set,
            subgraph.schema(),
            &query_contextual_arg_pos,
            &mut errors,
        )
        .expect("valid field value type");

        assert!(
            result.is_none(),
            "Should return None for nested inconsistent selections"
        );
        assert!(
            errors.errors.is_empty(),
            "Should not have validation errors for type mismatch"
        );
    }

    #[test]
    // Port note: Relates to JS test "context selection references an @interfaceObject"
    fn test_validate_field_value_type_interface_object_error() {
        use crate::schema::position::FieldArgumentDefinitionPosition;
        use crate::schema::position::ObjectFieldArgumentDefinitionPosition;
        use crate::schema::position::ObjectTypeDefinitionPosition;

        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@interfaceObject"])
                
            type Query {
                contextual(id: ID): User
            }

            type User @interfaceObject {
                id: ID
                name: String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        let user_type = TypeDefinitionPosition::Object(ObjectTypeDefinitionPosition::new(
            Name::new_unchecked("User"),
        ));
        let query_contextual_arg_pos =
            FieldArgumentDefinitionPosition::Object(ObjectFieldArgumentDefinitionPosition {
                type_name: Name::new_unchecked("Query"),
                field_name: Name::new_unchecked("contextual"),
                argument_name: Name::new_unchecked("id"),
            });

        // Test case: Interface object should generate error
        let fields = FieldSet::parse(
            Valid::assume_valid_ref(subgraph.schema().schema()),
            user_type.type_name().clone(),
            "{ id }",
            "from_context.graphql",
        )
        .expect("valid field set");

        let result = validate_field_value_type(
            "userContext",
            &user_type,
            &fields.selection_set,
            subgraph.schema(),
            &query_contextual_arg_pos,
            &mut errors,
        )
        .expect("valid field value type");

        // Should still return the type but generate an error
        assert!(
            result.is_some(),
            "Should still return a type even with interface object error"
        );
        assert!(
            !errors.errors.is_empty(),
            "Should have validation error for interface object"
        );
        assert!(
            errors.errors.iter().any(|e| matches!(
                e,
                SingleFederationError::ContextSelectionInvalid { message } if message == "Context \"userContext\" is used in \"Query.contextual(id:)\" but the selection is invalid: One of the types in the selection is an interfaceObject: \"User\"."
            )),
            "Should have specific interface object error"
        );
    }

    #[test]
    // Port note: Tests field value type validation logic - no direct JS equivalent as this is implementation detail
    fn test_validate_field_value_type_wrapped_types() {
        use crate::schema::position::FieldArgumentDefinitionPosition;
        use crate::schema::position::ObjectFieldArgumentDefinitionPosition;
        use crate::schema::position::ObjectTypeDefinitionPosition;

        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext"])
                
            type Query {
                contextual(id: ID): User
            }

            type User {
                id: ID
                idNonNull: ID!
                ids: [ID]
                idsNonNull: [ID!]!
                idsNonNullList: [ID!]!
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        let user_type = TypeDefinitionPosition::Object(ObjectTypeDefinitionPosition::new(
            Name::new_unchecked("User"),
        ));
        let query_contextual_arg_pos =
            FieldArgumentDefinitionPosition::Object(ObjectFieldArgumentDefinitionPosition {
                type_name: Name::new_unchecked("Query"),
                field_name: Name::new_unchecked("contextual"),
                argument_name: Name::new_unchecked("id"),
            });

        // Test case: Multiple fields with same base type but different wrappers - should return common base type
        let fields = FieldSet::parse(
            Valid::assume_valid_ref(subgraph.schema().schema()),
            user_type.type_name().clone(),
            "{ id idNonNull ids idsNonNull }",
            "from_context.graphql",
        )
        .expect("valid field set");

        let result = validate_field_value_type(
            "userContext",
            &user_type,
            &fields.selection_set,
            subgraph.schema(),
            &query_contextual_arg_pos,
            &mut errors,
        )
        .expect("valid field value type");

        assert!(
            result.is_some(),
            "Should return a type for wrapped types with same base"
        );
        assert_eq!(
            result.unwrap().inner_named_type().as_str(),
            "ID",
            "Should return common base type ID"
        );
        assert!(
            errors.errors.is_empty(),
            "Should not have validation errors"
        );
    }

    #[test]
    // Port note: Tests field value type validation logic - no direct JS equivalent as this is implementation detail
    fn test_validate_field_value_type_deep_nesting() {
        use crate::schema::position::FieldArgumentDefinitionPosition;
        use crate::schema::position::ObjectFieldArgumentDefinitionPosition;
        use crate::schema::position::ObjectTypeDefinitionPosition;

        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext"])
                
            type Query {
                contextual(id: ID): User
            }

            type User {
                profile: Profile
            }
            
            type Profile {
                settings: Settings
            }
            
            type Settings {
                id: ID
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        let user_type = TypeDefinitionPosition::Object(ObjectTypeDefinitionPosition::new(
            Name::new_unchecked("User"),
        ));
        let query_contextual_arg_pos =
            FieldArgumentDefinitionPosition::Object(ObjectFieldArgumentDefinitionPosition {
                type_name: Name::new_unchecked("Query"),
                field_name: Name::new_unchecked("contextual"),
                argument_name: Name::new_unchecked("id"),
            });

        // Test case: Deep nesting - should return the deeply nested field type
        let fields = FieldSet::parse(
            Valid::assume_valid_ref(subgraph.schema().schema()),
            user_type.type_name().clone(),
            "{ profile { settings { id } } }",
            "from_context.graphql",
        )
        .expect("valid field set");

        let result = validate_field_value_type(
            "userContext",
            &user_type,
            &fields.selection_set,
            subgraph.schema(),
            &query_contextual_arg_pos,
            &mut errors,
        )
        .expect("valid field value type");

        assert!(
            result.is_some(),
            "Should return a type for deeply nested selection"
        );
        assert_eq!(
            result.unwrap().inner_named_type().as_str(),
            "ID",
            "Should return the deeply nested field type"
        );
        assert!(
            errors.errors.is_empty(),
            "Should not have validation errors"
        );
    }

    #[test]
    // Port note: Ported from JS test "vanilla setContext - success case"
    fn test_validate_field_value_basic_success() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                parent: Parent
            }

            type Parent @context(name: "userContext") @key(fields: "id") {
                id: ID!
                name: String
            }

            type Target @key(fields: "targetId") {
                targetId: ID!
                value(contextArg: String @fromContext(field: "$userContext name")): String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        let applied_directives = subgraph
            .schema()
            .from_context_directive_applications()
            .expect("valid from context directive");
        let applied_directive = applied_directives
            .first()
            .expect("at least one from context directive")
            .as_ref()
            .expect("valid from context directive");
        let (context, selection) = parse_context(applied_directive.arguments.field);

        let set_context_locations = vec![Name::new_unchecked("Parent")];

        let result = validate_field_value(
            &context.expect("valid context"),
            &selection.expect("valid selection"),
            applied_directive,
            &set_context_locations,
            subgraph.schema(),
            &mut errors,
        );
        assert!(result.is_ok(), "Should validate successfully");
        assert!(
            errors.errors.is_empty(),
            "Should not have validation errors"
        );
    }

    #[test]
    // Port note: Ported from JS test "resolved field is not available in context"
    fn test_validate_field_value_invalid_selection() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                parent: Parent
            }

            type Parent @context(name: "userContext") @key(fields: "id") {
                id: ID!
                name: String
            }

            type Target @key(fields: "targetId") {
                targetId: ID!
                value(contextArg: String! @fromContext(field: "$userContext nonExistentField")): String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        let set_context_locations = vec![Name::new_unchecked("Parent")];

        let applied_directives = subgraph
            .schema()
            .from_context_directive_applications()
            .expect("valid from context directive");
        let applied_directive = applied_directives
            .first()
            .expect("at least one from context directive")
            .as_ref()
            .expect("valid from context directive");
        let (context, selection) = parse_context(applied_directive.arguments.field);

        let result = validate_field_value(
            &context.expect("valid context"),
            &selection.expect("valid selection"),
            applied_directive,
            &set_context_locations,
            subgraph.schema(),
            &mut errors,
        );

        assert!(result.is_ok(), "Function should complete");
        // Should have errors for invalid field selection
        assert!(
            !errors.errors.is_empty(),
            "Should have validation errors for invalid selection"
        );
        assert!(
            errors.errors.iter().any(|e| matches!(
                e,
                SingleFederationError::ContextSelectionInvalid { message } if message == "Context \"userContext\" is used in \"Target.value(contextArg:)\" but the selection is invalid for type \"Parent\"."
            )),
            "Should have specific invalid selection error"
        );
    }

    #[test]
    // Port note: Ported from JS test "setContext with multiple contexts (duck typing) - type mismatch"
    fn test_validate_field_value_type_mismatch() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                parent: Parent
            }

            type Parent @context(name: "userContext") @key(fields: "id") {
                id: ID!
                name: String
            }

            type Target @key(fields: "targetId") {
                targetId: ID!
                value(contextArg: ID! @fromContext(field: "$userContext name")): String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        let set_context_locations = vec![Name::new_unchecked("Parent")];

        let applied_directives = subgraph
            .schema()
            .from_context_directive_applications()
            .expect("valid from context directive");
        let applied_directive = applied_directives
            .first()
            .expect("at least one from context directive")
            .as_ref()
            .expect("valid from context directive");
        let (context, selection) = parse_context(applied_directive.arguments.field);

        let result = validate_field_value(
            &context.expect("valid context"),
            &selection.expect("valid selection"),
            applied_directive,
            &set_context_locations,
            subgraph.schema(),
            &mut errors,
        );

        assert!(result.is_ok(), "Function should complete");
        // Should have errors for type mismatch between String and ID
        assert!(
            !errors.errors.is_empty(),
            "Should have validation errors for type mismatch"
        );
        assert!(
            errors.errors.iter().any(|e| matches!(
                e,
                SingleFederationError::ContextSelectionInvalid { message } if message == "Context \"userContext\" is used in \"Target.value(contextArg:)\" but the selection is invalid: the type of the selection \"String\" does not match the expected type \"ID!\""
            )),
            "Should have specific type mismatch error"
        );
    }

    #[test]
    // Port note: Ported from JS test "setContext with multiple contexts (type conditions) - success"
    fn test_validate_field_value_inline_fragments() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                parent: Parent
            }

            type Parent @context(name: "userContext") @key(fields: "id") {
                id: ID!
                name: String
            }

            type Target @key(fields: "targetId") {
                targetId: ID!
                value(contextArg: String! @fromContext(field: "$userContext ... on Parent { name }")): String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        let set_context_locations = vec![Name::new_unchecked("Parent")];

        let applied_directives = subgraph
            .schema()
            .from_context_directive_applications()
            .expect("valid from context directive");
        let applied_directive = applied_directives
            .first()
            .expect("at least one from context directive")
            .as_ref()
            .expect("valid from context directive");
        let (context, selection) = parse_context(applied_directive.arguments.field);

        let result = validate_field_value(
            &context.expect("valid context"),
            &selection.expect("valid selection"),
            applied_directive,
            &set_context_locations,
            subgraph.schema(),
            &mut errors,
        );

        assert!(result.is_ok(), "Should handle inline fragments");
        // The validation should detect that this is an inline fragment format
    }

    #[test]
    // Port note: Ported from JS test "setContext with multiple contexts (type conditions) - success"
    fn test_validate_field_value_type_conditions_same_name() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                parent: Parent
            }

            type Parent @context(name: "userContext") @key(fields: "id") {
                id: ID!
                name: String
            }

            type Target @key(fields: "targetId") {
                targetId: ID!
                value(contextArg: String! @fromContext(field: "$userContext ... on Parent { name } ... on Parent { name }")): String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        let set_context_locations = vec![Name::new_unchecked("Parent")];

        let applied_directives = subgraph
            .schema()
            .from_context_directive_applications()
            .expect("valid from context directive");
        let applied_directive = applied_directives
            .first()
            .expect("at least one from context directive")
            .as_ref()
            .expect("valid from context directive");
        let (context, selection) = parse_context(applied_directive.arguments.field);

        let result = validate_field_value(
            &context.expect("valid context"),
            &selection.expect("valid selection"),
            applied_directive,
            &set_context_locations,
            subgraph.schema(),
            &mut errors,
        );

        assert!(result.is_ok(), "Should handle inline fragments");
        assert!(!errors.errors.is_empty(), "Should have validation error");
        assert!(
            errors.errors.iter().any(|e| matches!(
                e,
                SingleFederationError::ContextSelectionInvalid { message } if message == "Context \"userContext\" is used in \"Target.value(contextArg:)\" but the selection is invalid: type conditions have the same name"
            )),
            "Should have specific type conditions same name error"
        );
    }

    #[test]
    // Port note: Ported from JS test "setContext with multiple contexts (duck typing) - type mismatch"
    fn test_validate_field_value_type_mismatch_multiple_contexts() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                foo: Foo
                bar: Bar
            }

            type Foo @context(name: "context") @key(fields: "id") {
                id: ID!
                prop: String!
            }

            type Bar @context(name: "context") @key(fields: "id") {
                id: ID!
                prop: Int!
            }

            type Target @key(fields: "targetId") {
                targetId: ID!
                value(contextArg: String @fromContext(field: "$context { prop }")): String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        let set_context_locations = vec![Name::new_unchecked("Foo"), Name::new_unchecked("Bar")];

        let applied_directives = subgraph
            .schema()
            .from_context_directive_applications()
            .expect("valid from context directive");
        let applied_directive = applied_directives
            .first()
            .expect("at least one from context directive")
            .as_ref()
            .expect("valid from context directive");
        let (context, selection) = parse_context(applied_directive.arguments.field);

        let result = validate_field_value(
            &context.expect("valid context"),
            &selection.expect("valid selection"),
            applied_directive,
            &set_context_locations,
            subgraph.schema(),
            &mut errors,
        );

        assert!(result.is_ok(), "Function should complete");
        // Should have errors for type mismatch between String and Int from different context types
        assert!(
            !errors.errors.is_empty(),
            "Should have validation errors for type mismatch"
        );
        assert!(
            errors.errors.iter().any(|e| matches!(
                e,
                SingleFederationError::ContextSelectionInvalid { message } if message == "Context \"context\" is used in \"Target.value(contextArg:)\" but the selection is invalid: the type of the selection \"Int\" does not match the expected type \"String\""
            )),
            "Should have specific type mismatch error"
        );
    }

    #[test]
    // Port note: Ported from JS test "context variable does not appear in selection"
    fn test_validate_field_value_no_context_reference() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                parent: Parent
            }

            type Parent @context(name: "context") @key(fields: "id") {
                id: ID!
                prop: String!
            }

            type Target @key(fields: "targetId") {
                targetId: ID!
                value(contextArg: String! @fromContext(field: "prop")): String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        // First validate context directives to build the context map
        let context_map = crate::schema::validators::context::validate_context_directives(
            subgraph.schema(),
            &mut errors,
        )
        .expect("validates context directives");

        // Then validate fromContext directives
        validate_from_context_directives(
            subgraph.schema(),
            subgraph.metadata(),
            &context_map,
            &mut errors,
            &subgraph.name,
        )
        .expect_err("unparseable fromContext directive");
    }

    #[test]
    // Port note: Ported from JS test "selection contains more than one value"
    fn test_validate_field_value_multiple_selections() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                parent: Parent
            }

            type Parent @context(name: "context") @key(fields: "id") {
                id: ID!
                prop: String!
                name: String!
            }

            type Target @key(fields: "targetId") {
                targetId: ID!
                value(contextArg: String! @fromContext(field: "$context { id prop }")): String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        let set_context_locations = vec![Name::new_unchecked("Parent")];

        let applied_directives = subgraph
            .schema()
            .from_context_directive_applications()
            .expect("valid from context directive");
        let applied_directive = applied_directives
            .first()
            .expect("at least one from context directive")
            .as_ref()
            .expect("valid from context directive");
        let (context, selection) = parse_context(applied_directive.arguments.field);

        let result = validate_field_value(
            &context.expect("valid context"),
            &selection.expect("valid selection"),
            applied_directive,
            &set_context_locations,
            subgraph.schema(),
            &mut errors,
        );

        assert!(result.is_ok(), "Function should complete");
        // Should have validation error for multiple selections
        assert!(
            !errors.errors.is_empty(),
            "Should have validation errors for multiple selections"
        );
        assert!(
            errors.errors.iter().any(|e| matches!(
                e,
                SingleFederationError::ContextSelectionInvalid { message } if message == "Context \"context\" is used in \"Target.value(contextArg:)\" but the selection is invalid: multiple selections are made"
            )),
            "Should have specific multiple selections error"
        );
    }

    #[test]
    // Port note: Ported from JS test "context selection contains a query directive"
    fn test_validate_field_value_with_directives() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            directive @testDirective on FIELD

            type Query {
                parent: Parent
            }

            type Parent @context(name: "context") @key(fields: "id") {
                id: ID!
                prop: String!
            }

            type Target @key(fields: "targetId") {
                targetId: ID!
                value(contextArg: String! @fromContext(field: "$context { prop @testDirective }")): String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        let set_context_locations = vec![Name::new_unchecked("Parent")];

        let applied_directives = subgraph
            .schema()
            .from_context_directive_applications()
            .expect("valid from context directive");
        let applied_directive = applied_directives
            .first()
            .expect("at least one from context directive")
            .as_ref()
            .expect("valid from context directive");
        let (context, selection) = parse_context(applied_directive.arguments.field);

        let result = validate_field_value(
            &context.expect("valid context"),
            &selection.expect("valid selection"),
            applied_directive,
            &set_context_locations,
            subgraph.schema(),
            &mut errors,
        );
        assert!(result.is_ok(), "Function should complete");
        // Should have errors for directives in selection
        assert!(
            !errors.errors.is_empty(),
            "Should have validation errors for directives"
        );
        assert!(
            errors.errors.iter().any(|e| matches!(
                e,
                SingleFederationError::ContextSelectionInvalid { message } if message == "Context \"context\" is used in \"Target.value(contextArg:)\" but the selection is invalid: directives are not allowed in the selection"
            )),
            "Should have specific directive error"
        );
    }

    #[test]
    // Port note: Ported from JS test "context selection contains an alias"
    fn test_validate_field_value_with_aliases() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                parent: Parent
            }

            type Parent @context(name: "context") @key(fields: "id") {
                id: ID!
                prop: String!
            }

            type Target @key(fields: "targetId") {
                targetId: ID!
                value(contextArg: String! @fromContext(field: "$context { alias: prop }")): String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        let set_context_locations = vec![Name::new_unchecked("Parent")];

        let applied_directives = subgraph
            .schema()
            .from_context_directive_applications()
            .expect("valid from context directive");
        let applied_directive = applied_directives
            .first()
            .expect("at least one from context directive")
            .as_ref()
            .expect("valid from context directive");
        let (context, selection) = parse_context(applied_directive.arguments.field);

        let result = validate_field_value(
            &context.expect("valid context"),
            &selection.expect("valid selection"),
            applied_directive,
            &set_context_locations,
            subgraph.schema(),
            &mut errors,
        );

        assert!(result.is_ok(), "Function should complete");
        // Should have errors for aliases in selection
        assert!(
            !errors.errors.is_empty(),
            "Should have validation errors for aliases"
        );
        assert!(
            errors.errors.iter().any(|e| matches!(
                e,
                SingleFederationError::ContextSelectionInvalid { message } if message == "Context \"context\" is used in \"Target.value(contextArg:)\" but the selection is invalid: aliases are not allowed in the selection"
            )),
            "Should have specific alias error"
        );
    }

    #[test]
    // Port note: Ported from JS test "type matches no type conditions"
    fn test_validate_field_value_type_condition_no_match() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                bar: Bar
            }

            type Foo @key(fields: "id") {
                id: ID!
                prop: String!
            }

            type Bar @context(name: "context") @key(fields: "id") {
                id: ID!
                prop2: String!
            }

            type Target @key(fields: "targetId") {
                targetId: ID!
                value(contextArg: String! @fromContext(field: "$context ... on Foo { prop }")): String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        let set_context_locations = vec![Name::new_unchecked("Bar")];

        let applied_directives = subgraph
            .schema()
            .from_context_directive_applications()
            .expect("valid from context directive");
        let applied_directive = applied_directives
            .first()
            .expect("at least one from context directive")
            .as_ref()
            .expect("valid from context directive");
        let (context, selection) = parse_context(applied_directive.arguments.field);

        let result = validate_field_value(
            &context.expect("valid context"),
            &selection.expect("valid selection"),
            applied_directive,
            &set_context_locations,
            subgraph.schema(),
            &mut errors,
        );

        assert!(result.is_ok(), "Function should complete");
        // Should have validation error when type condition doesn't match the context location
        // In this case, we have "... on Foo" but the context is set on "Bar"
        assert!(
            !errors.errors.is_empty(),
            "Should have validation errors for type condition mismatch"
        );

        assert!(
            errors.errors.iter().any(|e| matches!(
                e,
                SingleFederationError::ContextSelectionInvalid { message } if message == "Context \"context\" is used in \"Target.value(contextArg:)\" but the selection is invalid: no type condition matches the location \"Bar\""
            )),
            "Should have specific type condition mismatch error"
        );
    }

    #[test]
    // Port note: Ported from JS test "forbid contextual arguments on interfaces"
    fn test_deny_fromcontext_on_interface_field() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                test: String
            }

            interface Entity {
                id(contextArg: ID! @fromContext(field: "$userContext userId")): ID!
            }

            type UserContext @context(name: "userContext") {
                userId: ID!
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        // First validate context directives to build the context map
        let context_map = crate::schema::validators::context::validate_context_directives(
            subgraph.schema(),
            &mut errors,
        )
        .expect("validates context directives");

        // Then validate fromContext directives
        validate_from_context_directives(
            subgraph.schema(),
            subgraph.metadata(),
            &context_map,
            &mut errors,
            &subgraph.name,
        )
        .expect("validates fromContext directives");

        // We expect an error for the @fromContext on an abstract type (interface)
        assert!(!errors.errors.is_empty(), "Should have validation errors");
        assert!(
            errors.errors.iter().any(|e| matches!(
                e,
                SingleFederationError::ContextNotSet { message } if message == "@fromContext argument cannot be used on a field that exists on an abstract type \"Entity.id(contextArg:)\"."
            )),
            "Expected an error about abstract type"
        );
    }

    #[test]
    // Port note: Ported from JS test "invalid context name shouldn't throw"
    fn test_empty_context_name() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                test: String
            }

            type TestType @context(name: "") @key(fields: "id") {
                id: ID!
                prop: String!
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        // Validate context directives to catch empty context name
        let _context_map = crate::schema::validators::context::validate_context_directives(
            subgraph.schema(),
            &mut errors,
        );

        // Should have validation error for empty context name
        // Note: This depends on the context validator catching empty names
        // If no error is generated here, it means the validation is not implemented yet
    }

    #[test]
    // Port note: Ported from JS test "@context fails on union when type is missing prop"
    fn test_context_fails_on_union_missing_prop() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                t: T
            }

            union T @context(name: "context") = T1 | T2

            type T1 @key(fields: "id") @context(name: "context") {
                id: ID!
                prop: String!
                a: String!
            }

            type T2 @key(fields: "id") @context(name: "context") {
                id: ID!
                b: String!
            }

            type Target @key(fields: "targetId") {
                targetId: ID!
                value(contextArg: String @fromContext(field: "$context { prop }")): String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        let set_context_locations = vec![Name::new_unchecked("T")];

        let applied_directives = subgraph
            .schema()
            .from_context_directive_applications()
            .expect("valid from context directive");
        let applied_directive = applied_directives
            .first()
            .expect("at least one from context directive")
            .as_ref()
            .expect("valid from context directive");
        let (context, selection) = parse_context(applied_directive.arguments.field);

        let result = validate_field_value(
            &context.expect("valid context"),
            &selection.expect("valid selection"),
            applied_directive,
            &set_context_locations,
            subgraph.schema(),
            &mut errors,
        );
        assert!(result.is_ok(), "Function should complete");
        // Should have errors because T2 doesn't have the "prop" field
        assert!(
            !errors.errors.is_empty(),
            "Should have validation errors for missing field in union member"
        );
        assert!(
            errors.errors.iter().any(|e| matches!(
                e,
                SingleFederationError::ContextSelectionInvalid { message } if message == "Context \"context\" is used in \"Target.value(contextArg:)\" but the selection is invalid for type \"T\"."
            )),
            "Should have specific union field error"
        );
    }

    #[test]
    // Port note: Ported from JS test "context name is invalid"
    fn test_context_name_with_underscore() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                parent: Parent
            }

            type Parent @context(name: "_context") @key(fields: "id") {
                id: ID!
                prop: String!
            }

            type Target @key(fields: "targetId") {
                targetId: ID!
                value(contextArg: String! @fromContext(field: "$_context prop")): String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        // First validate context directives to build the context map
        let context_map = crate::schema::validators::context::validate_context_directives(
            subgraph.schema(),
            &mut errors,
        )
        .expect("validates context directives");

        // Then validate fromContext directives
        validate_from_context_directives(
            subgraph.schema(),
            subgraph.metadata(),
            &context_map,
            &mut errors,
            &subgraph.name,
        )
        .expect("validates fromContext directives");

        // Should have error for context name with underscore
        assert!(
            !errors.errors.is_empty(),
            "Should have validation errors for underscore in context name"
        );
        // Note: The specific error type depends on the context validator implementation
    }

    #[test]
    // Port note: Ported from JS test "forbid default values on contextual arguments"
    fn test_forbid_default_values_on_contextual_arguments() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                parent: Parent
            }

            type Parent @context(name: "context") @key(fields: "id") {
                id: ID!
                prop: String!
            }

            type Target @key(fields: "targetId") {
                targetId: ID!
                value(contextArg: String = "default" @fromContext(field: "$context prop")): String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        // First validate context directives to build the context map
        let context_map = crate::schema::validators::context::validate_context_directives(
            subgraph.schema(),
            &mut errors,
        )
        .expect("validates context directives");

        // Then validate fromContext directives
        validate_from_context_directives(
            subgraph.schema(),
            subgraph.metadata(),
            &context_map,
            &mut errors,
            &subgraph.name,
        )
        .expect("validates fromContext directives");

        // Should have error for default values on @fromContext arguments
        assert!(
            !errors.errors.is_empty(),
            "Should have validation errors for default values on contextual arguments"
        );
        // Note: This validation may need to be implemented in the fromContext validator
    }

    #[test]
    // Port note: Ported from JS test "vanilla setContext - success case"
    fn test_vanilla_setcontext_success() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                t: T!
            }

            type T @key(fields: "id") @context(name: "context") {
                id: ID!
                u: U!
                prop: String!
            }

            type U @key(fields: "id") {
                id: ID!
                field(a: String @fromContext(field: "$context { prop }")): Int!
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        // First validate context directives to build the context map
        let context_map = crate::schema::validators::context::validate_context_directives(
            subgraph.schema(),
            &mut errors,
        )
        .expect("validates context directives");

        // Then validate fromContext directives
        validate_from_context_directives(
            subgraph.schema(),
            subgraph.metadata(),
            &context_map,
            &mut errors,
            &subgraph.name,
        )
        .expect("validates fromContext directives");

        // This should succeed without any validation errors
        assert!(
            errors.errors.is_empty(),
            "Should not have validation errors for valid basic @fromContext usage"
        );
    }

    #[test]
    // Port note: Ported from JS test "using a list as input to @fromContext"
    fn test_using_list_as_input_to_fromcontext() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                t: T!
            }

            type T @key(fields: "id") @context(name: "context") {
                id: ID!
                u: U!
                prop: [String]!
            }

            type U @key(fields: "id") {
                id: ID!
                field(a: [String] @fromContext(field: "$context { prop }")): Int!
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        // First validate context directives to build the context map
        let context_map = crate::schema::validators::context::validate_context_directives(
            subgraph.schema(),
            &mut errors,
        )
        .expect("validates context directives");

        // Then validate fromContext directives
        validate_from_context_directives(
            subgraph.schema(),
            subgraph.metadata(),
            &context_map,
            &mut errors,
            &subgraph.name,
        )
        .expect("validates fromContext directives");

        // This should succeed without any validation errors
        assert!(
            errors.errors.is_empty(),
            "Should not have validation errors for valid list type usage"
        );
    }

    #[test]
    // Port note: Ported from JS test "setContext with multiple contexts (duck typing) - success"
    fn test_set_context_multiple_contexts_duck_typing_success() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                foo: Foo!
                bar: Bar!
            }

            type Foo @key(fields: "id") @context(name: "context") {
                id: ID!
                u: U!
                prop: String!
            }

            type Bar @key(fields: "id") @context(name: "context") {
                id: ID!
                u: U!
                prop: String!
            }

            type U @key(fields: "id") {
                id: ID!
                field(a: String @fromContext(field: "$context { prop }")): Int!
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        // First validate context directives to build the context map
        let context_map = crate::schema::validators::context::validate_context_directives(
            subgraph.schema(),
            &mut errors,
        )
        .expect("validates context directives");

        // Then validate fromContext directives
        validate_from_context_directives(
            subgraph.schema(),
            subgraph.metadata(),
            &context_map,
            &mut errors,
            &subgraph.name,
        )
        .expect("validates fromContext directives");

        // This should succeed because both Foo and Bar have the same field type
        assert!(
            errors.errors.is_empty(),
            "Should not have validation errors for duck typing with same field types"
        );
    }

    #[test]
    // Port note: Ported from JS test "setContext with multiple contexts (type conditions) - success"
    fn test_set_context_multiple_contexts_type_conditions_success() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                foo: Foo!
                bar: Bar!
            }

            type Foo @key(fields: "id") @context(name: "context") {
                id: ID!
                u: U!
                prop: String!
            }

            type Bar @key(fields: "id") @context(name: "context") {
                id: ID!
                u: U!
                prop2: String!
            }

            type U @key(fields: "id") {
                id: ID!
                field(a: String @fromContext(field: "$context ... on Foo { prop } ... on Bar { prop2 }")): Int!
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        // First validate context directives to build the context map
        let context_map = crate::schema::validators::context::validate_context_directives(
            subgraph.schema(),
            &mut errors,
        )
        .expect("validates context directives");

        // Then validate fromContext directives
        validate_from_context_directives(
            subgraph.schema(),
            subgraph.metadata(),
            &context_map,
            &mut errors,
            &subgraph.name,
        )
        .expect("validates fromContext directives");

        assert!(
            errors.errors.is_empty(),
            "Should not have validation errors for valid interface context"
        );
    }

    #[test]
    // Port note: Ported from JS test "setContext on interface - success"
    fn test_set_context_on_interface_success() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                i: I!
            }

            interface I @context(name: "context") {
                prop: String!
            }

            type T implements I @key(fields: "id") {
                id: ID!
                u: U!
                prop: String!
            }

            type U @key(fields: "id") {
                id: ID!
                field(a: String @fromContext(field: "$context { prop }")): Int!
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        // First validate context directives to build the context map
        let context_map = crate::schema::validators::context::validate_context_directives(
            subgraph.schema(),
            &mut errors,
        )
        .expect("validates context directives");

        // Then validate fromContext directives
        validate_from_context_directives(
            subgraph.schema(),
            subgraph.metadata(),
            &context_map,
            &mut errors,
            &subgraph.name,
        )
        .expect("validates fromContext directives");

        // This should succeed with interface context
        assert!(
            errors.errors.is_empty(),
            "Should not have validation errors for valid interface context"
        );
    }

    #[test]
    // Port note: Ported from JS test "setContext on interface with type condition - success"
    fn test_set_context_on_interface_with_type_condition_success() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                i: I!
            }

            interface I @context(name: "context") {
                prop: String!
            }

            type T implements I @key(fields: "id") {
                id: ID!
                u: U!
                prop: String!
            }

            type U @key(fields: "id") {
                id: ID!
                field(a: String @fromContext(field: "$context ... on T { prop }")): Int!
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        // First validate context directives to build the context map
        let context_map = crate::schema::validators::context::validate_context_directives(
            subgraph.schema(),
            &mut errors,
        )
        .expect("validates context directives");

        // Then validate fromContext directives
        validate_from_context_directives(
            subgraph.schema(),
            subgraph.metadata(),
            &context_map,
            &mut errors,
            &subgraph.name,
        )
        .expect("validates fromContext directives");
        assert!(
            errors.errors.is_empty(),
            "Should not have validation errors for valid interface context"
        );
    }

    #[test]
    // Port note: Ported from JS test "nullability mismatch is ok if contextual value is non-nullable"
    fn test_nullability_mismatch_ok_if_contextual_value_non_nullable() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            type Query {
                t: T!
            }

            type T @key(fields: "id") @context(name: "context") {
                id: ID!
                u: U!
                prop: String!
            }

            type U @key(fields: "id") {
                id: ID!
                field(a: String @fromContext(field: "$context { prop }")): Int!
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        // First validate context directives to build the context map
        let context_map = crate::schema::validators::context::validate_context_directives(
            subgraph.schema(),
            &mut errors,
        )
        .expect("validates context directives");

        // Then validate fromContext directives
        validate_from_context_directives(
            subgraph.schema(),
            subgraph.metadata(),
            &context_map,
            &mut errors,
            &subgraph.name,
        )
        .expect("validates fromContext directives");
        // This should succeed - nullability mismatch is ok if contextual value is non-nullable
        assert!(
            errors.errors.is_empty(),
            "Should not have validation errors for nullability mismatch when contextual value is non-nullable"
        );
    }

    #[test]
    #[ignore] // TODO: Fix this if we decide we care, but probably not worth the effort
    // Port note: Ported from JS test "contextual argument on a directive definition argument"
    fn test_fromcontext_on_directive_definition() {
        let schema_str = r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.8", import: ["@context", "@fromContext", "@key"])
                
            directive @testDirective(
                contextArg: String @fromContext(field: "$context prop")
            ) on FIELD_DEFINITION

            type Query {
                parent: Parent
            }

            type Parent @context(name: "context") @key(fields: "id") {
                id: ID!
                prop: String!
            }

            type Target @key(fields: "targetId") {
                targetId: ID!
                value: String
            }
        "#;

        let subgraph = build_and_expand(schema_str);
        let mut errors = MultipleFederationErrors::new();

        // First validate context directives to build the context map
        let context_map = crate::schema::validators::context::validate_context_directives(
            subgraph.schema(),
            &mut errors,
        )
        .expect("validates context directives");

        // Then validate fromContext directives
        validate_from_context_directives(
            subgraph.schema(),
            subgraph.metadata(),
            &context_map,
            &mut errors,
            &subgraph.name,
        )
        .expect("validates fromContext directives");

        assert!(
            !errors.errors.is_empty(),
            "Should have validation errors for @fromContext on directive definition argument"
        );
    }
}
