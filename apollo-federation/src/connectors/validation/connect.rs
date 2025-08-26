//! Parsing and validation of `@connect` directives

use std::collections::HashMap;
use std::fmt;
use std::ops::Range;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::schema::ExtendedType;
use hashbrown::HashSet;
use itertools::Itertools;
use multi_try::MultiTry;

use self::entity::validate_entity_arg;
use self::selection::Selection;
use super::Code;
use super::Message;
use super::coordinates::ConnectDirectiveCoordinate;
use super::errors::ErrorsCoordinate;
use super::errors::IsSuccessArgument;
use crate::connectors::Namespace;
use crate::connectors::SourceName;
use crate::connectors::id::ConnectedElement;
use crate::connectors::id::ObjectCategory;
use crate::connectors::schema_type_ref::SchemaTypeRef;
use crate::connectors::spec::connect::CONNECT_ID_ARGUMENT_NAME;
use crate::connectors::spec::connect::CONNECT_SOURCE_ARGUMENT_NAME;
use crate::connectors::spec::source::SOURCE_NAME_ARGUMENT_NAME;
use crate::connectors::validation::connect::http::Http;
use crate::connectors::validation::errors::Errors;
use crate::connectors::validation::graphql::SchemaInfo;

mod entity;
mod http;
mod selection;

pub(super) fn fields_seen_by_all_connects(
    schema: &SchemaInfo,
    all_source_names: &[SourceName],
) -> Result<Vec<(Name, Name)>, Vec<Message>> {
    let mut messages = Vec::new();
    let mut connects = Vec::new();

    for (type_name, extended_type) in schema.types.iter().filter(|(_, ty)| !ty.is_built_in()) {
        // Only check types that can have connectors (objects, interfaces, unions)
        if matches!(
            extended_type,
            ExtendedType::Object(_) | ExtendedType::Interface(_) | ExtendedType::Union(_)
        ) && let Some(type_ref) = SchemaTypeRef::new(schema.schema, type_name)
        {
            let (connects_for_type, messages_for_type) =
                Connect::find_on_type(type_ref, schema, all_source_names);
            connects.extend(connects_for_type);
            messages.extend(messages_for_type);
        }
    }

    let mut seen_fields = Vec::new();
    let mut valid_id_names: HashMap<_, Vec<_>> = HashMap::new();
    for connect in connects {
        if let Some(name) = connect.id.and_then(|value| value.as_str()) {
            match Name::new(name) {
                Ok(name) => {
                    valid_id_names.entry(name).or_insert_with(Vec::new).push(
                        connect
                            .id
                            .and_then(|node| node.line_column_range(&schema.sources)),
                    );
                }
                Err(err) => {
                    let locations = connect
                        .id
                        .and_then(|node| node.line_column_range(&schema.sources))
                        .map(|loc| vec![loc])
                        .unwrap_or_default();
                    messages.push(Message {
                        code: Code::InvalidConnectorIdName,
                        message: err.to_string(),
                        locations,
                    });
                }
            }
        }
        match connect.type_check() {
            Ok(seen_fields_for_connect) => {
                seen_fields.extend(
                    seen_fields_for_connect
                        .into_iter()
                        .map(|field| (field.object_name, field.field_name)),
                );
            }
            Err(messages_for_connect) => {
                messages.extend(messages_for_connect);
            }
        }
    }

    let non_unique_errors = valid_id_names
        .into_iter()
        .map(|(name, locations)| {
            (
                name,
                locations
                    .into_iter()
                    .flatten()
                    .collect::<Vec<Range<LineColumn>>>(),
            )
        })
        .filter(|(_, locations)| locations.len() > 1)
        .map(|(name, locations)| Message {
            code: Code::DuplicateIdName,
            message: format!(
                "`@connector` directive must have unique `id`. `{name}` has {} repetitions",
                locations.len()
            ),
            locations,
        });
    messages.extend(non_unique_errors);

    if messages.is_empty() {
        Ok(seen_fields)
    } else {
        Err(messages)
    }
}

/// A parsed `@connect` directive
struct Connect<'schema> {
    selection: Selection<'schema>,
    http: Http<'schema>,
    errors: Errors<'schema>,
    is_success: Option<IsSuccessArgument<'schema>>,
    coordinate: ConnectDirectiveCoordinate<'schema>,
    schema: &'schema SchemaInfo<'schema>,
    id: Option<&'schema Node<Value>>,
}

impl<'schema> Connect<'schema> {
    /// Find and parse any `@connect` directives on this type or its fields.
    fn find_on_type(
        type_ref: SchemaTypeRef<'schema>,
        schema: &'schema SchemaInfo,
        source_names: &'schema [SourceName],
    ) -> (Vec<Self>, Vec<Message>) {
        let object_category = if schema
            .schema_definition
            .query
            .as_ref()
            .is_some_and(|query| query.name == *type_ref.name())
        {
            ObjectCategory::Query
        } else if schema
            .schema_definition
            .mutation
            .as_ref()
            .is_some_and(|mutation| mutation.name == *type_ref.name())
        {
            ObjectCategory::Mutation
        } else {
            ObjectCategory::Other
        };

        let directives_on_type = match type_ref.extended() {
            ExtendedType::Object(obj) => obj.directives.iter(),
            ExtendedType::Interface(iface) => iface.directives.iter(),
            ExtendedType::Union(union) => union.directives.iter(),
            _ => [].iter(), // Other types (scalars, enums, etc.) don't support connectors
        }
        .filter(|directive| directive.name == *schema.connect_directive_name())
        .map(|directive| ConnectDirectiveCoordinate {
            directive,
            element: ConnectedElement::Type { type_ref },
        });

        let directives_on_fields = match type_ref.extended() {
            ExtendedType::Object(obj) => obj
                .fields
                .values()
                .flat_map(|field| {
                    field
                        .directives
                        .iter()
                        .filter(|directive| directive.name == *schema.connect_directive_name())
                        .map(|directive| ConnectDirectiveCoordinate {
                            directive,
                            element: ConnectedElement::Field {
                                parent_type: type_ref,
                                parent_category: object_category,
                                field_def: field,
                            },
                        })
                })
                .collect::<Vec<_>>(),
            ExtendedType::Interface(iface) => iface
                .fields
                .values()
                .flat_map(|field| {
                    field
                        .directives
                        .iter()
                        .filter(|directive| directive.name == *schema.connect_directive_name())
                        .map(|directive| ConnectDirectiveCoordinate {
                            directive,
                            element: ConnectedElement::Field {
                                parent_type: type_ref,
                                parent_category: object_category,
                                field_def: field,
                            },
                        })
                })
                .collect::<Vec<_>>(),
            ExtendedType::Union(_) => {
                // Unions don't have fields, so no field-level directives
                Vec::new()
            }
            _ => Vec::new(),
        }
        .into_iter();

        let (connects, messages): (Vec<Connect>, Vec<Vec<Message>>) = directives_on_type
            .chain(directives_on_fields)
            .map(|coordinate| Self::parse(coordinate, schema, source_names))
            .partition_result();

        let messages: Vec<Message> = messages.into_iter().flatten().collect();

        (connects, messages)
    }

    /// Parse the `@connect` directive and run just enough checks to be able to use it at runtime.
    /// More advanced checks are done in [`Self::type_check`].
    ///
    /// Three sub-pieces are parsed:
    /// 1. `@connect(http:)` with [`Http::parse`]
    /// 2. `@connect(source:)` with [`validate_source_name`]
    /// 3. `@connect(selection:)` with [`Selection::parse`]
    ///
    /// `selection` and `source` are _always_ checked and their errors are returned.
    /// The order these two run in doesn't matter.
    /// `http` can't be validated without knowing whether a `source` was set, so it's only checked if `source` is valid.
    fn parse(
        coordinate: ConnectDirectiveCoordinate<'schema>,
        schema: &'schema SchemaInfo,
        source_names: &[SourceName],
    ) -> Result<Self, Vec<Message>> {
        if coordinate.element.is_root_type(schema) {
            return Err(vec![Message {
                code: Code::ConnectOnRoot,
                message: format!(
                    "Cannot use `@{connect_directive_name}` on root types like `{object_name}`",
                    object_name = coordinate.element.base_type_name(),
                    connect_directive_name = schema.connect_directive_name(),
                ),
                locations: coordinate
                    .directive
                    .line_column_range(&schema.sources)
                    .into_iter()
                    .collect(),
            }]);
        }

        let (selection, http, errors, is_success) = Selection::parse(coordinate, schema)
            .map_err(|err| vec![err])
            .and_try(
                validate_source_name(coordinate, source_names, schema)
                    .map_err(|err| vec![err])
                    .and_then(|source_name| Http::parse(coordinate, source_name.as_ref(), schema)),
            )
            .and_try(Errors::parse(
                ErrorsCoordinate::Connect {
                    connect: coordinate,
                },
                schema,
            ))
            .and_try(
                IsSuccessArgument::parse_for_connector(coordinate, schema).map_err(|err| vec![err]),
            )
            .map_err(|nested| nested.into_iter().flatten().collect_vec())?;

        let id = coordinate
            .directive
            .argument_by_name(CONNECT_ID_ARGUMENT_NAME.as_str(), schema)
            // ID Argument is optional
            .map(Some)
            .unwrap_or_default();

        Ok(Self {
            selection,
            http,
            errors,
            is_success,
            coordinate,
            schema,
            id,
        })
    }

    fn type_check(self) -> Result<Vec<ResolvedField>, Vec<Message>> {
        let mut messages = Vec::new();

        let all_variables = self
            .selection
            .variables()
            .chain(self.http.variables())
            .chain(self.errors.variables())
            .collect::<HashSet<_>>();
        if all_variables.contains(&Namespace::Batch) && all_variables.contains(&Namespace::This) {
            messages.push(Message {
                code: Code::ConnectBatchAndThis,
                message: format!(
                    "In {}: connectors cannot use both $this and $batch",
                    self.coordinate
                ),
                locations: self
                    .coordinate
                    .directive
                    .line_column_range(&self.schema.sources)
                    .into_iter()
                    .collect(),
            });
        }

        messages.extend(validate_entity_arg(self.coordinate, self.schema).err());
        messages.extend(
            self.http
                .type_check(self.schema)
                .err()
                .into_iter()
                .flatten(),
        );
        messages.extend(
            self.errors
                .type_check(self.schema)
                .err()
                .into_iter()
                .flatten(),
        );

        if let Some(is_success_argument) = self.is_success {
            messages.extend(is_success_argument.type_check(self.schema).err());
        }

        let mut seen: Vec<ResolvedField> = match self.selection.type_check(self.schema) {
            // TODO: use ResolvedField struct at all levels
            Ok(seen) => seen
                .into_iter()
                .map(|(object_name, field_name)| ResolvedField {
                    object_name,
                    field_name,
                })
                .collect(),
            Err(message) => {
                messages.push(message);
                return Err(messages);
            }
        };

        if let ConnectedElement::Field {
            parent_type,
            field_def,
            ..
        } = self.coordinate.element
        {
            // mark the field with a @connect directive as seen
            seen.push(ResolvedField {
                object_name: parent_type.name().clone(),
                field_name: field_def.name.clone(),
            });
            // direct recursion isn't allowed, like a connector on User.friends: [User]
            if parent_type.name() == field_def.ty.inner_named_type().as_str() {
                messages.push(Message {
                    code: Code::CircularReference,
                    message: format!(
                        "Direct circular reference detected in `{}.{}: {}`. For more information, see https://go.apollo.dev/connectors/limitations#circular-references",
                        parent_type.name().clone(),
                        field_def.name,
                        field_def.ty
                    ),
                    locations: field_def.line_column_range(&self.schema.sources).into_iter().collect(),
                });
            }
        }

        if messages.is_empty() {
            Ok(seen)
        } else {
            Err(messages)
        }
    }
}

/// A field that is resolved by a connect directive
pub(super) struct ResolvedField {
    pub object_name: Name,
    pub field_name: Name,
}

fn validate_source_name(
    coordinate: ConnectDirectiveCoordinate,
    source_names: &[SourceName],
    schema: &SchemaInfo,
) -> Result<Option<SourceName>, Message> {
    let Some(source_name) = SourceName::from_connect(coordinate.directive) else {
        return Ok(None);
    };

    if source_names.contains(&source_name) {
        return Ok(Some(source_name));
    }
    // A source name was set but doesn't match a defined source
    // TODO: Pick a suggestion that's not just the first defined source
    let qualified_directive = ConnectSourceCoordinate {
        connect: coordinate,
        source: source_name.as_str(),
    };
    if let Some(first_source_name) = source_names.first() {
        Err(Message {
            code: Code::SourceNameMismatch,
            message: format!(
                "{qualified_directive} does not match any defined sources. Did you mean \"{first_source_name}\"?",
                first_source_name = first_source_name.as_str(),
            ),
            locations: source_name.locations(&schema.sources),
        })
    } else {
        Err(Message {
            code: Code::NoSourcesDefined,
            message: format!(
                "{qualified_directive} specifies a source, but none are defined. Try adding `@{source_directive_name}({SOURCE_NAME_ARGUMENT_NAME}: \"{value}\")` to the schema.",
                source_directive_name = schema.source_directive_name(),
                value = source_name,
            ),
            locations: source_name.locations(&schema.sources),
        })
    }
}

struct ConnectSourceCoordinate<'schema> {
    source: &'schema str,
    connect: ConnectDirectiveCoordinate<'schema>,
}
impl fmt::Display for ConnectSourceCoordinate<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`@{connect_directive_name}({CONNECT_SOURCE_ARGUMENT_NAME}: \"{source}\")` on `{element}`",
            connect_directive_name = self.connect.directive.name,
            element = self.connect.element,
            source = self.source,
        )
    }
}
