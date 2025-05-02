//! Parsing and validation of `@connect` directives

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;
use hashbrown::HashSet;
use itertools::Itertools;
use multi_try::MultiTry;

use self::entity::validate_entity_arg;
use self::selection::Selection;
use super::Code;
use super::Message;
use super::coordinates::ConnectDirectiveCoordinate;
use super::coordinates::connect_directive_name_coordinate;
use super::coordinates::source_name_value_coordinate;
use super::source::SourceName;
use crate::sources::connect::Namespace;
use crate::sources::connect::id::ConnectedElement;
use crate::sources::connect::id::ObjectCategory;
use crate::sources::connect::spec::schema::CONNECT_SOURCE_ARGUMENT_NAME;
use crate::sources::connect::validation::connect::http::Http;
use crate::sources::connect::validation::graphql::SchemaInfo;

mod entity;
mod http;
mod selection;

pub(super) fn fields_seen_by_all_connects(
    schema: &SchemaInfo,
    all_source_names: &[SourceName],
) -> Result<Vec<(Name, Name)>, Vec<Message>> {
    let mut messages = Vec::new();
    let mut connects = Vec::new();

    for extended_type in schema.types.values().filter(|ty| !ty.is_built_in()) {
        let ExtendedType::Object(node) = extended_type else {
            continue;
        };
        let (connects_for_type, messages_for_type) =
            Connect::find_on_type(node, schema, all_source_names);
        connects.extend(connects_for_type);
        messages.extend(messages_for_type);
    }

    let mut seen_fields = Vec::new();
    for connect in connects {
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
    coordinate: ConnectDirectiveCoordinate<'schema>,
    schema: &'schema SchemaInfo<'schema>,
}

impl<'schema> Connect<'schema> {
    /// Find and parse any `@connect` directives on this type or its fields.
    fn find_on_type(
        object: &'schema Node<ObjectType>,
        schema: &'schema SchemaInfo,
        source_names: &'schema [SourceName],
    ) -> (Vec<Self>, Vec<Message>) {
        let object_category = if schema
            .schema_definition
            .query
            .as_ref()
            .is_some_and(|query| query.name == object.name)
        {
            ObjectCategory::Query
        } else if schema
            .schema_definition
            .mutation
            .as_ref()
            .is_some_and(|mutation| mutation.name == object.name)
        {
            ObjectCategory::Mutation
        } else {
            ObjectCategory::Other
        };

        let directives_on_type = object
            .directives
            .iter()
            .filter(|directive| directive.name == *schema.connect_directive_name())
            .map(|directive| ConnectDirectiveCoordinate {
                directive,
                element: ConnectedElement::Type { type_def: object },
            });

        let directives_on_fields = object.fields.values().flat_map(|field| {
            field
                .directives
                .iter()
                .filter(|directive| directive.name == *schema.connect_directive_name())
                .map(|directive| ConnectDirectiveCoordinate {
                    directive,
                    element: ConnectedElement::Field {
                        parent_type: object,
                        parent_category: object_category,
                        field_def: field,
                    },
                })
        });

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
        source_names: &'schema [SourceName],
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

        let (selection, http) = Selection::parse(coordinate, schema)
            .map_err(|err| vec![err])
            .and_try(
                validate_source_name(&coordinate, source_names, schema)
                    .map_err(|err| vec![err])
                    .and_then(|source_name| Http::parse(coordinate, source_name, schema)),
            )
            .map_err(|nested| nested.into_iter().flatten().collect_vec())?;

        Ok(Self {
            selection,
            http,
            coordinate,
            schema,
        })
    }

    fn type_check(self) -> Result<Vec<ResolvedField>, Vec<Message>> {
        let mut messages = Vec::new();

        let all_variables = self
            .selection
            .variables()
            .chain(self.http.variables())
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
                object_name: parent_type.name.clone(),
                field_name: field_def.name.clone(),
            });
            // direct recursion isn't allowed, like a connector on User.friends: [User]
            if &parent_type.name == field_def.ty.inner_named_type() {
                messages.push(Message {
                    code: Code::CircularReference,
                    message: format!(
                        "Direct circular reference detected in `{}.{}: {}`. For more information, see https://go.apollo.dev/connectors/limitations#circular-references",
                        parent_type.name,
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

fn validate_source_name<'schema>(
    coordinate: &ConnectDirectiveCoordinate,
    source_names: &'schema [SourceName],
    schema: &SchemaInfo,
) -> Result<Option<&'schema SourceName<'schema>>, Message> {
    let Some(source_name_arg) = coordinate
        .directive
        .arguments
        .iter()
        .find(|arg| arg.name == CONNECT_SOURCE_ARGUMENT_NAME)
    else {
        return Ok(None);
    };

    let resolved_source_name = source_names
        .iter()
        .find(|name| **name == source_name_arg.value);

    if let Some(source_name) = resolved_source_name {
        return Ok(Some(source_name));
    }
    // A source name was set but doesn't match a defined source
    // TODO: Pick a suggestion that's not just the first defined source
    let qualified_directive = connect_directive_name_coordinate(
        schema.connect_directive_name(),
        &source_name_arg.value,
        coordinate,
    );
    if let Some(first_source_name) = source_names.first() {
        Err(Message {
            code: Code::SourceNameMismatch,
            message: format!(
                "{qualified_directive} does not match any defined sources. Did you mean \"{first_source_name}\"?",
                first_source_name = first_source_name.as_str(),
            ),
            locations: source_name_arg
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        })
    } else {
        Err(Message {
            code: Code::NoSourcesDefined,
            message: format!(
                "{qualified_directive} specifies a source, but none are defined. Try adding {coordinate} to the schema.",
                coordinate = source_name_value_coordinate(
                    schema.source_directive_name(),
                    &source_name_arg.value
                ),
            ),
            locations: source_name_arg
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        })
    }
}
