//! Validations that check the entire connectors schema together:

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::ast::Directive;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::executable::Selection;
use apollo_compiler::name;
use apollo_compiler::parser::Parser;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::parser::SourceSpan;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::validation::Valid;
use itertools::Itertools;

use self::keys::EntityKeyChecker;
use self::keys::field_set_error;
use crate::link::Import;
use crate::link::Link;
use crate::link::federation_spec_definition::FEDERATION_FIELDS_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_RESOLVABLE_ARGUMENT_NAME;
use crate::link::spec::Identity;
use crate::sources::connect::Connector;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::validation::graphql::SchemaInfo;
use crate::subgraph::spec::CONTEXT_DIRECTIVE_NAME;
use crate::subgraph::spec::EXTERNAL_DIRECTIVE_NAME;
use crate::subgraph::spec::FROM_CONTEXT_DIRECTIVE_NAME;
mod keys;

pub(super) fn validate(
    schema: &SchemaInfo,
    file_name: &str,
    fields_seen_by_connectors: Vec<(Name, Name)>,
) -> Vec<Message> {
    let messages: Vec<Message> = check_for_disallowed_type_definitions(schema)
        .chain(check_conflicting_directives(schema))
        .collect();
    if !messages.is_empty() {
        return messages;
    }
    check_seen_fields(schema, fields_seen_by_connectors)
        .chain(advanced_validations(schema, file_name))
        .collect()
}

fn check_for_disallowed_type_definitions(schema: &SchemaInfo) -> impl Iterator<Item = Message> {
    let subscription_name = schema
        .schema_definition
        .subscription
        .as_ref()
        .map(|sub| &sub.name);
    schema
        .types
        .values()
        .filter_map(move |extended_type| match extended_type {
            ExtendedType::Union(union_type) => Some(abstract_type_error(
                SourceSpan::recompose(union_type.location(), union_type.name.location()),
                &schema.sources,
                "union",
            )),
            ExtendedType::Interface(interface) => Some(abstract_type_error(
                SourceSpan::recompose(interface.location(), interface.name.location()),
                &schema.sources,
                "interface",
            )),
            ExtendedType::Object(obj) if subscription_name.is_some_and(|name| name == &obj.name) => {
                    Some(Message {
                        code: Code::SubscriptionInConnectors,
                        message: format!(
                            "A subscription root type is not supported when using `@{connect_directive_name}`.",
                            connect_directive_name = schema.connect_directive_name(),
                        ),
                        locations: obj.name.line_column_range(&schema.sources).into_iter().collect(),
                    })
            }
            _ => None,
        })
}

/// Certain federation directives are not allowed when using connectors.
/// We produce errors for any which were imported, even if not used.
fn check_conflicting_directives(schema: &Schema) -> Vec<Message> {
    let Some((fed_link, fed_link_directive)) =
        Link::for_identity(schema, &Identity::federation_identity())
    else {
        return Vec::new();
    };

    // TODO: make the `Link` code retain locations directly instead of reparsing stuff for validation
    let imports = fed_link_directive
        .specified_argument_by_name(&name!("import"))
        .and_then(|arg| arg.as_list())
        .into_iter()
        .flatten()
        .filter_map(|value| Import::from_value(value).ok().map(|import| (value, import)))
        .collect_vec();

    let disallowed_imports = [CONTEXT_DIRECTIVE_NAME, FROM_CONTEXT_DIRECTIVE_NAME];
    fed_link
        .imports
        .into_iter()
        .filter_map(|import| {
            disallowed_imports
                .contains(&import.element)
                .then(|| Message {
                    code: Code::ConnectorsUnsupportedFederationDirective,
                    message: format!(
                        "The directive `@{import}` is not supported when using connectors.",
                        import = import.alias.as_ref().unwrap_or(&import.element)
                    ),
                    locations: imports
                        .iter()
                        .find_map(|(value, reparsed)| {
                            (*reparsed == *import).then(|| value.line_column_range(&schema.sources))
                        })
                        .flatten()
                        .into_iter()
                        .collect(),
                })
        })
        .collect()
}

fn abstract_type_error(node: Option<SourceSpan>, source_map: &SourceMap, keyword: &str) -> Message {
    Message {
        code: Code::ConnectorsUnsupportedAbstractType,
        message: format!(
            "Abstract schema types, such as `{keyword}`, are not supported when using connectors. You can check out our documentation at https://go.apollo.dev/connectors/best-practices#abstract-schema-types-are-unsupported."
        ),
        locations: node
            .and_then(|location| location.line_column_range(source_map))
            .into_iter()
            .collect(),
    }
}

/// Check that all fields defined in the schema are resolved by a connector.
fn check_seen_fields(
    schema: &SchemaInfo,
    fields_seen_by_connectors: Vec<(Name, Name)>,
) -> impl Iterator<Item = Message> {
    let federation = Link::for_identity(schema, &Identity::federation_identity());
    let external_directive_name = federation
        .map(|(link, _)| link.directive_name_in_schema(&EXTERNAL_DIRECTIVE_NAME))
        .unwrap_or(EXTERNAL_DIRECTIVE_NAME);

    let all_fields: IndexSet<_> = schema
        .types
        .values()
        .filter_map(|extended_type| {
            if extended_type.is_built_in() {
                return None;
            }
            let coord = |(name, _): (&Name, _)| (extended_type.name().clone(), name.clone());

            // ignore all fields on objects marked @external
            if extended_type
                .directives()
                .iter()
                .any(|dir| dir.name == external_directive_name)
            {
                return None;
            }

            match extended_type {
                ExtendedType::Object(object) => {
                    // ignore fields marked @external
                    Some(
                        object
                            .fields
                            .iter()
                            .filter(|(_, def)| {
                                !def.directives
                                    .iter()
                                    .any(|dir| dir.name == external_directive_name)
                            })
                            .map(coord),
                    )
                }
                ExtendedType::Interface(_) => None, // TODO: when interfaces are supported (probably should include fields from implementing/member types as well)
                _ => None,
            }
        })
        .flatten()
        .collect();

    let mut seen_fields = fields_seen_by_resolvable_keys(schema);
    seen_fields.extend(fields_seen_by_connectors);

    (&all_fields - &seen_fields).into_iter().map(move |(parent_type, field_name)| {
        let Ok(field_def) = schema.type_field(&parent_type, &field_name) else {
            // This should never happen, but if it does, we don't want to panic
            return Message {
                code: Code::GraphQLError,
                message: format!(
                    "Field `{parent_type}.{field_name}` is missing from the schema.",
                ),
                locations: Vec::new(),
            };
        };
        Message {
            code: Code::ConnectorsUnresolvedField,
            message: format!(
                "No connector resolves field `{parent_type}.{field_name}`. It must have a `@{connect_directive_name}` directive or appear in `@{connect_directive_name}(selection:)`.",
                connect_directive_name = schema.connect_directive_name()
            ),
            locations: field_def.line_column_range(&schema.sources).into_iter().collect(),
        }
    })
}

fn fields_seen_by_resolvable_keys(schema: &SchemaInfo) -> IndexSet<(Name, Name)> {
    let mut seen_fields = IndexSet::default();
    let objects = schema.types.values().filter_map(|node| node.as_object());
    // Mark resolvable key fields as seen
    let mut selections: Vec<(Name, Selection)> = objects
        .clone()
        .flat_map(|object| {
            resolvable_key_fields(object, schema).flat_map(|(field_set, _)| {
                field_set
                    .selection_set
                    .selections
                    .iter()
                    .map(|selection| (object.name.clone(), selection.clone()))
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    while !selections.is_empty() {
        if let Some((type_name, selection)) = selections.pop() {
            if let Some(field) = selection.as_field() {
                let t = (type_name, field.name.clone());
                if !seen_fields.contains(&t) {
                    seen_fields.insert(t);
                    field.selection_set.selections.iter().for_each(|selection| {
                        selections.push((field.ty().inner_named_type().clone(), selection.clone()));
                    });
                }
            }
        }
    }

    seen_fields
}

/// For an object type, get all the keys (and directive nodes) that are resolvable.
///
/// The FieldSet returned here is what goes in the `fields` argument, so `id` in `@key(fields: "id")`
fn resolvable_key_fields<'a>(
    object: &'a ObjectType,
    schema: &'a Schema,
) -> impl Iterator<Item = (FieldSet, &'a Component<Directive>)> {
    object
        .directives
        .iter()
        .filter(|directive| directive.name == FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC)
        .filter(|directive| {
            directive
                .arguments
                .iter()
                .find(|arg| arg.name == FEDERATION_RESOLVABLE_ARGUMENT_NAME)
                .and_then(|arg| arg.value.to_bool())
                .unwrap_or(true)
        })
        .filter_map(|directive| {
            if let Some(fields_str) = directive
                .arguments
                .iter()
                .find(|arg| arg.name == FEDERATION_FIELDS_ARGUMENT_NAME)
                .map(|arg| &arg.value)
                .and_then(|value| value.as_str())
            {
                Parser::new()
                    .parse_field_set(
                        Valid::assume_valid_ref(schema),
                        object.name.clone(),
                        fields_str.to_string(),
                        "",
                    )
                    .ok()
                    .map(|field_set| (field_set, directive))
            } else {
                None
            }
        })
}

fn advanced_validations(schema: &SchemaInfo, subgraph_name: &str) -> Vec<Message> {
    let mut messages = Vec::new();

    let Ok(connectors) = Connector::from_schema(schema, subgraph_name, schema.connect_link.spec)
    else {
        return messages;
    };

    let mut entity_checker = EntityKeyChecker::default();

    for (field_set, directive) in find_all_resolvable_keys(schema) {
        entity_checker.add_key(&field_set, directive);
    }

    for (_, connector) in connectors {
        match connector.resolvable_key(schema) {
            Ok(None) => continue,
            Err(_) => {
                let variables = connector.variable_references().collect_vec();
                messages.push(field_set_error(
                    &variables,
                    &connector.id.directive.coordinate(),
                ))
            }
            Ok(Some(field_set)) => {
                entity_checker.add_connector(field_set);
            }
        }
    }

    if !messages.is_empty() {
        // Don't produce errors about unresolved keys if we _know_ some of the generated keys are wrong
        return messages;
    }

    entity_checker.check_for_missing_entity_connectors(schema)
}

fn find_all_resolvable_keys(schema: &Schema) -> Vec<(FieldSet, &Component<Directive>)> {
    schema
        .types
        .values()
        .flat_map(|extended_type| match extended_type {
            ExtendedType::Object(object) => Some(resolvable_key_fields(object, schema)),
            _ => None,
        })
        .flatten()
        .collect()
}
