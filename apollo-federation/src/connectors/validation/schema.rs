//! Validations that check the entire connectors schema together:

use std::ops::Range;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::ast::Directive;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::executable::Selection;
use apollo_compiler::name;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::parser::Parser;
use apollo_compiler::parser::SourceSpan;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use hashbrown::HashSet;
use indexmap::IndexMap;
use itertools::Itertools;
use shape::Shape;
use shape::ShapeCase;
use shape::ShapeVisitor;

use self::keys::EntityKeyChecker;
use self::keys::field_set_error;
pub(crate) use self::keys::field_set_is_subset;
use crate::connectors::Connector;
use crate::connectors::EntityResolver::TypeBatch;
use crate::connectors::Namespace::Batch;
use crate::connectors::json_selection::SelectionTrie;
use crate::connectors::validation::Code;
use crate::connectors::validation::Message;
use crate::connectors::validation::graphql::SchemaInfo;
use crate::link::Import;
use crate::link::Link;
use crate::link::spec::Identity;
use crate::schema::HasFields;
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
    use crate::connectors::ConnectSpec;

    let subscription_name = schema
        .schema_definition
        .subscription
        .as_ref()
        .map(|sub| &sub.name);
    let spec = schema.connect_link.spec;

    user_defined_types(schema)
        .filter_map(move |(_, extended_type)| match extended_type {
            ExtendedType::Union(union_type) if spec < ConnectSpec::V0_4 => {
                Some(Message {
                    code: Code::ConnectorsUnsupportedAbstractType,
                    message: "Abstract schema types, such as `union`, are not supported when using connectors.".to_string(),
                    locations: SourceSpan::recompose(union_type.location(), union_type.name.location())
                        .and_then(|location| location.line_column_range(&schema.sources))
                        .into_iter()
                        .collect(),
                })
            }
            ExtendedType::Interface(interface_type) if spec < ConnectSpec::V0_4 => {
                Some(Message {
                    code: Code::ConnectorsUnsupportedAbstractType,
                    message: "Abstract schema types, such as `interface`, are not supported when using connectors.".to_string(),
                    locations: SourceSpan::recompose(interface_type.location(), interface_type.name.location())
                        .and_then(|location| location.line_column_range(&schema.sources))
                        .into_iter()
                        .collect(),
                })
            }
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
    // Not `LinksMetadata::for_identity`: that returns only the `Link`, and the messages below need
    // the `@link` AST node to locate the offending `import` entry. See the TODO just below.
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
        .filter_map(|value| {
            Import::try_from(value.as_ref())
                .ok()
                .map(|import| (value, import))
        })
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

/// The types the subgraph author actually wrote.
///
/// Connectors validation runs on an expanded schema, so by this point the schema also contains the
/// federation and connect spec definitions plus the generated `_Entity`/`_Service` types. Injected
/// elements are built programmatically and so carry no source location, while anything parsed from
/// the author's document does — that is what distinguishes them.
fn user_defined_types<'a>(
    schema: &'a SchemaInfo,
) -> impl Iterator<Item = (&'a Name, &'a ExtendedType)> {
    schema.types.iter().filter(|(_, extended_type)| {
        !extended_type.is_built_in() && is_user_defined(extended_type.location())
    })
}

/// Whether an element came from the author's document rather than from link expansion.
///
/// See [`user_defined_types`].
fn is_user_defined(location: Option<SourceSpan>) -> bool {
    location.is_some()
}

/// Check that all fields defined in the schema are resolved by a connector.
fn check_seen_fields(
    schema: &SchemaInfo,
    fields_seen_by_connectors: Vec<(Name, Name)>,
) -> impl Iterator<Item = Message> {
    // Resolved through the link metadata computed during expansion, so `@external` is found under
    // whatever name the author imported it as, without re-scanning and re-parsing every `@link`.
    let external_directive_name = schema
        .federation_schema()
        .metadata()
        .and_then(|metadata| metadata.for_identity(&Identity::federation_identity()))
        .map_or(EXTERNAL_DIRECTIVE_NAME, |link| {
            link.directive_name_in_schema(&EXTERNAL_DIRECTIVE_NAME)
        });

    let mut all_fields = IndexSet::default();

    // Collect fields from all types the author wrote
    for (_, extended_type) in user_defined_types(schema) {
        // ignore all fields on types marked @external
        if extended_type
            .directives()
            .iter()
            .any(|dir| dir.name == external_directive_name)
        {
            continue;
        }

        match extended_type {
            ExtendedType::Object(object) => {
                // Add object fields (ignore fields marked @external)
                for (field_name, field_def) in &object.fields {
                    if !is_user_defined(field_def.location()) {
                        continue;
                    }
                    if !field_def
                        .directives
                        .iter()
                        .any(|dir| dir.name == external_directive_name)
                    {
                        all_fields.insert((extended_type.name().clone(), field_name.clone()));
                    }
                }
            }
            ExtendedType::Interface(interface) => {
                // For interfaces, only add fields from implementing types
                // Interface fields are implicitly resolved when implementing types resolve them
                for (type_name, implementing_type) in user_defined_types(schema) {
                    if let ExtendedType::Object(obj) = implementing_type
                        && obj.implements_interfaces.contains(&interface.name)
                    {
                        for (field_name, field_def) in &obj.fields {
                            if !is_user_defined(field_def.location()) {
                                continue;
                            }
                            if !field_def
                                .directives
                                .iter()
                                .any(|dir| dir.name == external_directive_name)
                            {
                                all_fields.insert((type_name.clone(), field_name.clone()));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

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
    // Mark resolvable key fields as seen
    let mut selections: Vec<(Name, Selection)> = find_all_resolvable_keys(schema)
        .into_iter()
        .flat_map(|(field_set, _)| {
            let type_name = field_set.selection_set.ty.clone();
            field_set
                .selection_set
                .selections
                .iter()
                .map(|selection| (type_name.clone(), selection.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    while !selections.is_empty() {
        if let Some((type_name, selection)) = selections.pop()
            && let Some(field) = selection.as_field()
        {
            let t = (type_name, field.name.clone());
            if !seen_fields.contains(&t) {
                seen_fields.insert(t);
                field.selection_set.selections.iter().for_each(|selection| {
                    selections.push((field.ty().inner_named_type().clone(), selection.clone()));
                });
            }
        }
    }

    seen_fields
}

fn advanced_validations(schema: &SchemaInfo, subgraph_name: &str) -> Vec<Message> {
    let mut messages = Vec::new();

    let Ok(connectors) = Connector::from_schema(schema, subgraph_name) else {
        return messages;
    };

    let mut entity_checker = EntityKeyChecker::default();

    for (field_set, directive) in find_all_resolvable_keys(schema) {
        entity_checker.add_key(&field_set, directive);
    }

    for connector in &connectors {
        if connector.entity_resolver == Some(TypeBatch) {
            let input_trie = compute_batch_input_trie(connector);
            match SelectionSetWalker::new(connector.name(), schema, &input_trie)
                .walk(&connector.selection.shape(), connector)
            {
                Ok(res) => messages.extend(res),
                Err(err) => messages.push(err),
            }
        }
    }

    for connector in connectors {
        match connector.resolvable_key(schema) {
            Ok(None) => continue,
            Err(_) => {
                let variables = connector.variable_references().collect_vec();
                messages.push(field_set_error(&variables, &connector, schema));
            }
            Ok(Some(field_set)) => {
                entity_checker.add_connector(field_set, &connector.selection.shape());
            }
        }
    }

    if !messages.is_empty() {
        // Don't produce errors about unresolved keys if we _know_ some of the generated keys are wrong
        return messages;
    }

    entity_checker.check_for_missing_entity_connectors(schema)
}

fn compute_batch_input_trie(connector: &Connector) -> SelectionTrie {
    let mut trie = SelectionTrie::new();
    connector
        .variable_references()
        .filter(|var| var.namespace.namespace == Batch)
        .for_each(|var| {
            let _ = &trie.extend(&var.selection);
        });
    trie
}

struct SelectionSetWalker<'walker> {
    name: Name,
    schema: &'walker SchemaInfo<'walker>,
    trie: &'walker SelectionTrie,
    unmapped_fields: HashSet<String>,
}

impl<'walker> SelectionSetWalker<'walker> {
    fn new(name: Name, schema: &'walker SchemaInfo<'walker>, trie: &'walker SelectionTrie) -> Self {
        SelectionSetWalker {
            name,
            schema,
            trie,
            unmapped_fields: HashSet::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum ShapeVisitorError<'error> {
    #[error(
        "The `@connect` directive on `{connector}` specifies a `$batch` entity resolver, but the field `{unset}` could not be found in `@connect(selection: ...)`"
    )]
    BatchKeyNotSubsetOfOutputShape {
        connector: String,
        unset: &'error String,
        locations: Vec<Range<LineColumn>>,
    },
    #[error("Attempted to resolve key on unexpected shape `{shape_str}`")]
    UnexpectedKeyOnShape {
        shape_str: String,
        locations: Vec<Range<LineColumn>>,
    },
    #[error(
        "`$batch` fields must be mapped from the API response body. Variables such as `$context` and `$this` are not supported"
    )]
    NonRootBatch(Vec<Range<LineColumn>>),
}

impl From<ShapeVisitorError<'_>> for Message {
    fn from(value: ShapeVisitorError) -> Self {
        match &value {
            ShapeVisitorError::BatchKeyNotSubsetOfOutputShape { locations, .. } => Message {
                code: Code::ConnectorsBatchKeyNotInSelection,
                message: value.to_string(),
                locations: locations.clone(),
            },
            ShapeVisitorError::UnexpectedKeyOnShape { locations, .. } => Message {
                code: Code::ConnectorsUnresolvedField,
                message: value.to_string(),
                locations: locations.clone(),
            },
            ShapeVisitorError::NonRootBatch(locations) => Message {
                code: Code::ConnectorsNonRootBatchKey,
                message: value.to_string(),
                locations: locations.clone(),
            },
        }
    }
}

impl SelectionSetWalker<'_> {
    const ROOT_SHAPE: &'static str = "$root";

    fn walk(
        mut self,
        output_shape: &Shape,
        connector: &Connector,
    ) -> Result<Vec<Message>, Message> {
        output_shape.visit_shape(&mut self)?;

        // Collect messages from unset Names
        let mut vec = Vec::new();
        for unset in &self.unmapped_fields {
            vec.push(
                ShapeVisitorError::BatchKeyNotSubsetOfOutputShape {
                    connector: connector.id.directive.simple_name(),
                    unset,
                    locations: self
                        .name
                        .line_column_range(&self.schema.sources)
                        .into_iter()
                        .collect(),
                }
                .into(),
            );
        }
        Ok(vec)
    }
}
impl<'walker> ShapeVisitor for SelectionSetWalker<'walker> {
    type Error = ShapeVisitorError<'walker>;
    type Output = ();

    fn default(&mut self, shape: &Shape) -> Result<Self::Output, Self::Error> {
        Err(ShapeVisitorError::UnexpectedKeyOnShape {
            shape_str: shape.pretty_print(),
            locations: self
                .name
                .line_column_range(&self.schema.sources)
                .into_iter()
                .collect(),
        })
    }

    fn visit_object(
        &mut self,
        _: &Shape,
        fields: &IndexMap<String, Shape>,
        _: &Shape,
    ) -> Result<Self::Output, Self::Error> {
        for (key, sub_selection) in self.trie.iter() {
            // Object should contain all keys in the selection set.
            // If not, then the key is unmapped.
            let Some(next_shape) = fields.get(key) else {
                self.unmapped_fields.insert(key.to_string());
                continue;
            };

            // Check that next shape doesn't come from a non-`$root` field.
            if let ShapeCase::Name(name, _) = next_shape.case() {
                let base_name = name.base_shape_name();
                if base_name != Self::ROOT_SHAPE {
                    return Err(ShapeVisitorError::NonRootBatch(
                        self.name
                            .line_column_range(&self.schema.sources)
                            .into_iter()
                            .collect(),
                    ));
                }
            }

            // If key has no nested selections, then we can stop walking down this branch.
            if sub_selection.is_empty() {
                continue;
            }

            // Continue walking with nested selection sets
            let mut nested = SelectionSetWalker::new(self.name.clone(), self.schema, sub_selection);
            next_shape.visit_shape(&mut nested)?;
            self.unmapped_fields.extend(nested.unmapped_fields);
        }
        Ok(())
    }
}

/// Every resolvable `@key` the author wrote, paired with the AST node to report locations against.
///
/// The [`FieldSet`] is the parsed `fields` argument, so `id` in `@key(fields: "id")`.
///
/// This goes through [`FederationSchema::key_directive_applications`] rather than scanning for a
/// directive literally named `key`, so a subgraph that imports `@key` under an alias — or doesn't
/// import it at all, leaving it as `federation__key` — still has its keys found.
fn find_all_resolvable_keys<'a>(
    schema: &'a SchemaInfo,
) -> Vec<(FieldSet, &'a Component<Directive>)> {
    let Ok(applications) = schema.federation_schema().key_directive_applications() else {
        // No federation link, or `@key` has no definition. Nothing to check against.
        return Vec::new();
    };

    applications
        .into_iter()
        // Malformed applications are reported by the federation validators, not here.
        .filter_map(Result::ok)
        .filter(|key| key.resolvable())
        .filter(|key| is_user_defined(key.schema_directive().location()))
        .filter_map(|key| {
            let field_set = Parser::new()
                .parse_field_set(
                    Valid::assume_valid_ref(schema),
                    key.target().type_name().clone(),
                    key.fields().to_string(),
                    "",
                )
                .ok()?;
            Some((field_set, key.schema_directive()))
        })
        .collect()
}
