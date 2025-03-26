//! Parsing and validation of `@connect` directives

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;
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
use crate::sources::connect::ConnectSpec;
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
    let mut seen_fields = Vec::new();

    schema
        .types
        .values()
        .filter(|ty| !ty.is_built_in())
        .for_each(|extended_type| {
            if let ExtendedType::Object(node) = extended_type {
                match fields_seen_by_connectors_on_types(node, schema, all_source_names) {
                    Ok(fields) => seen_fields.extend(fields),
                    Err(errs) => messages.extend(errs),
                }

                match fields_seen_by_connectors_on_fields(node, schema, all_source_names) {
                    Ok(fields) => seen_fields.extend(fields),
                    Err(errs) => messages.extend(errs),
                }
            }
        });

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
        directive: &'schema Node<Directive>,
        element: ConnectedElement<'schema>,
        schema: &'schema SchemaInfo,
        source_names: &'schema [SourceName],
    ) -> Result<Self, Vec<Message>> {
        let coordinate = ConnectDirectiveCoordinate { directive, element };

        let (selection, http) = Selection::parse(coordinate, schema)
            .map_err(|err| vec![err])
            .and_try(
                validate_source_name(directive, &coordinate, source_names, schema)
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

        messages.extend(validate_entity_arg(self.coordinate, self.schema).err());
        messages.extend(
            self.http
                .type_check(self.schema)
                .err()
                .into_iter()
                .flatten(),
        );

        let seen = match self.selection.type_check(self.schema) {
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

/// Make sure that any `@connect` directives on types are valid
fn fields_seen_by_connectors_on_types(
    object: &Node<ObjectType>,
    schema: &SchemaInfo,
    source_names: &[SourceName],
) -> Result<Vec<(Name, Name)>, Vec<Message>> {
    let connect_directives = object
        .directives
        .iter()
        .filter(|directive| directive.name == *schema.connect_directive_name())
        .collect_vec();

    if connect_directives.is_empty() {
        return Ok(Vec::new());
    }

    // TODO: find a better place for feature gates like this
    if schema.connect_link.spec == ConnectSpec::V0_1 {
        return Err(vec![Message {
            code: Code::FeatureUnavailable,
            message: format!(
                "Using `@{connect_directive_name}` on `type {object_name}` requires connectors v0.2. Learn more at https://go.apollo.dev/connectors/changelog.",
                object_name = object.name,
                connect_directive_name = schema.connect_directive_name(),
            ),
            locations: object
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        }]);
    }

    let mut messages = Vec::new();
    let mut seen_fields = Vec::new();

    for directive in connect_directives {
        let element = ConnectedElement::Type { type_def: object };
        let connect = match Connect::parse(directive, element, schema, source_names) {
            Ok(connect) => connect,
            Err(errs) => {
                messages.extend(errs);
                continue;
            }
        };

        match connect.type_check() {
            Ok(resolved) => seen_fields.extend(
                resolved
                    .into_iter()
                    .map(|resolved| (resolved.object_name, resolved.field_name)),
            ),
            Err(errs) => messages.extend(errs),
        }
    }

    if messages.is_empty() {
        Ok(seen_fields)
    } else {
        Err(messages)
    }
}

/// Make sure that any `@connect` directives on object fields are valid
fn fields_seen_by_connectors_on_fields(
    object: &Node<ObjectType>,
    schema: &SchemaInfo,
    source_names: &[SourceName],
) -> Result<Vec<(Name, Name)>, Vec<Message>> {
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
    let mut seen_fields = Vec::new();
    let mut messages = Vec::new();
    for field in object.fields.values() {
        match fields_seen_by_connector(field, object_category, source_names, object, schema) {
            Ok(fields) => seen_fields.extend(fields),
            Err(errs) => messages.extend(errs),
        }
    }
    if messages.is_empty() {
        Ok(seen_fields)
    } else {
        Err(messages)
    }
}

fn fields_seen_by_connector(
    field: &Component<FieldDefinition>,
    category: ObjectCategory,
    source_names: &[SourceName],
    object: &Node<ObjectType>,
    schema: &SchemaInfo,
) -> Result<Vec<(Name, Name)>, Vec<Message>> {
    let source_map = &schema.sources;
    let mut messages = Vec::new();
    let connect_directives = field
        .directives
        .iter()
        .filter(|directive| directive.name == *schema.connect_directive_name())
        .collect_vec();

    if connect_directives.is_empty() {
        return Ok(Vec::new());
    }

    // mark the field with a @connect directive as seen
    let mut seen_fields = vec![(object.name.clone(), field.name.clone())];

    // direct recursion isn't allowed, like a connector on User.friends: [User]
    if &object.name == field.ty.inner_named_type() {
        messages.push(Message {
            code: Code::CircularReference,
            message: format!(
                "Direct circular reference detected in `{}.{}: {}`. For more information, see https://go.apollo.dev/connectors/limitations#circular-references",
                object.name,
                field.name,
                field.ty
            ),
            locations: field.line_column_range(source_map).into_iter().collect(),
        });
    }

    for directive in connect_directives {
        let element = ConnectedElement::Field {
            parent_type: object,
            parent_category: category,
            field_def: field,
        };
        let connect = match Connect::parse(directive, element, schema, source_names) {
            Ok(connect) => connect,
            Err(errs) => {
                messages.extend(errs);
                continue;
            }
        };
        match connect.type_check() {
            Ok(resolved) => seen_fields.extend(
                resolved
                    .into_iter()
                    .map(|resolved| (resolved.object_name, resolved.field_name)),
            ),
            Err(errs) => messages.extend(errs),
        }
    }

    if messages.is_empty() {
        Ok(seen_fields)
    } else {
        Err(messages)
    }
}

fn validate_source_name<'schema>(
    directive: &Node<Directive>,
    coordinate: &ConnectDirectiveCoordinate,
    source_names: &'schema [SourceName],
    schema: &SchemaInfo,
) -> Result<Option<&'schema SourceName<'schema>>, Message> {
    let Some(source_name_arg) = directive
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
