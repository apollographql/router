//! Implements API schema generation.
use crate::error::FederationError;
use crate::link::inaccessible_spec_definition::remove_inaccessible_elements;
use crate::link::inaccessible_spec_definition::validate_inaccessible;
use crate::schema::position;
use crate::schema::FederationSchema;
use apollo_compiler::name;
use apollo_compiler::schema::DirectiveDefinition;
use apollo_compiler::schema::DirectiveLocation;
use apollo_compiler::schema::InputValueDefinition;
use apollo_compiler::ty;
use apollo_compiler::validation::Valid;
use apollo_compiler::Node;
use apollo_compiler::Schema;

/// Remove types and directives imported by `@link`.
fn remove_core_feature_elements(schema: &mut FederationSchema) -> Result<(), FederationError> {
    let Some(metadata) = schema.metadata() else {
        return Ok(());
    };

    // First collect the things to be removed so we do not hold any immutable references
    // to the schema while mutating it below.
    let types_for_removal = schema
        .get_types()
        .filter(|position| metadata.source_link_of_type(position.type_name()).is_some())
        .collect::<Vec<_>>();

    let directives_for_removal = schema
        .get_directive_definitions()
        .filter(|position| {
            metadata
                .source_link_of_directive(&position.directive_name)
                .is_some()
        })
        .collect::<Vec<_>>();

    // First remove children of elements that need to be removed, so there won't be outgoing
    // references from the type.
    for position in &types_for_removal {
        match position {
            position::TypeDefinitionPosition::Object(position) => {
                let object = position.get(schema.schema())?;
                let remove_children = object
                    .fields
                    .keys()
                    .map(|field_name| position.field(field_name.clone()))
                    .collect::<Vec<_>>();
                for child in remove_children {
                    child.remove(schema)?;
                }
            }
            position::TypeDefinitionPosition::Interface(position) => {
                let interface = position.get(schema.schema())?;
                let remove_children = interface
                    .fields
                    .keys()
                    .map(|field_name| position.field(field_name.clone()))
                    .collect::<Vec<_>>();
                for child in remove_children {
                    child.remove(schema)?;
                }
            }
            position::TypeDefinitionPosition::InputObject(position) => {
                let input_object = position.get(schema.schema())?;
                let remove_children = input_object
                    .fields
                    .keys()
                    .map(|field_name| position.field(field_name.clone()))
                    .collect::<Vec<_>>();
                for child in remove_children {
                    child.remove(schema)?;
                }
            }
            position::TypeDefinitionPosition::Enum(position) => {
                let enum_ = position.get(schema.schema())?;
                let remove_children = enum_
                    .values
                    .keys()
                    .map(|field_name| position.value(field_name.clone()))
                    .collect::<Vec<_>>();
                for child in remove_children {
                    child.remove(schema)?;
                }
            }
            _ => {}
        }
    }

    for position in &directives_for_removal {
        position.remove(schema)?;
    }

    for position in &types_for_removal {
        match position {
            position::TypeDefinitionPosition::Object(position) => {
                position.remove(schema)?;
            }
            position::TypeDefinitionPosition::Interface(position) => {
                position.remove(schema)?;
            }
            position::TypeDefinitionPosition::InputObject(position) => {
                position.remove(schema)?;
            }
            position::TypeDefinitionPosition::Enum(position) => {
                position.remove(schema)?;
            }
            position::TypeDefinitionPosition::Scalar(position) => {
                position.remove(schema)?;
            }
            position::TypeDefinitionPosition::Union(position) => {
                position.remove(schema)?;
            }
        }
    }

    Ok(())
}

#[derive(Debug, Default, Clone)]
pub struct ApiSchemaOptions {
    pub include_defer: bool,
    pub include_stream: bool,
}

pub fn to_api_schema(
    schema: FederationSchema,
    options: ApiSchemaOptions,
) -> Result<Valid<Schema>, FederationError> {
    let mut api_schema = schema;

    // As we compute the API schema of a supergraph, we want to ignore explicit definitions of `@defer` and `@stream` because
    // those correspond to the merging of potential definitions from the subgraphs, but whether the supergraph API schema
    // supports defer or not is unrelated to whether subgraphs support it.
    if let Some(defer) = api_schema.get_directive_definition(&name!("defer")) {
        defer.remove(&mut api_schema)?;
    }
    if let Some(stream) = api_schema.get_directive_definition(&name!("stream")) {
        stream.remove(&mut api_schema)?;
    }

    validate_inaccessible(&api_schema)?;
    remove_inaccessible_elements(&mut api_schema)?;

    remove_core_feature_elements(&mut api_schema)?;

    let mut api_schema = api_schema.into_inner();

    if options.include_defer {
        api_schema
            .directive_definitions
            .insert(name!("defer"), defer_definition());
    }

    if options.include_stream {
        api_schema
            .directive_definitions
            .insert(name!("stream"), stream_definition());
    }

    crate::compat::make_print_schema_compatible(&mut api_schema);

    Ok(api_schema.validate()?)
}

fn defer_definition() -> Node<DirectiveDefinition> {
    Node::new(DirectiveDefinition {
        description: Some(
            r#"
The `@defer` directive may be provided for fragment spreads and inline fragments
to inform the executor to delay the execution of the current fragment to
indicate deprioritization of the current fragment. A query with `@defer`
directive will cause the request to potentially return multiple responses, where
non-deferred data is delivered in the initial response and data deferred is
delivered in a subsequent response. `@include` and `@skip` take precedence over
`@defer`.
        "#
            .trim()
            .into(),
        ),
        name: name!("defer"),
        arguments: vec![
            Node::new(InputValueDefinition {
                description: None,
                name: name!("label"),
                ty: ty!(String).into(),
                default_value: None,
                directives: Default::default(),
            }),
            Node::new(InputValueDefinition {
                description: None,
                name: name!("if"),
                ty: ty!(Boolean!).into(),
                default_value: Some(true.into()),
                directives: Default::default(),
            }),
        ],
        repeatable: false,
        locations: vec![DirectiveLocation::Field],
    })
}

fn stream_definition() -> Node<DirectiveDefinition> {
    Node::new(DirectiveDefinition {
        description: Some(
            r#"
The `@stream` directive may be provided for a field of `List` type so that the
backend can leverage technology such as asynchronous iterators to provide a
partial list in the initial response, and additional list items in subsequent
responses. `@include` and `@skip` take precedence over `@stream`.
        "#
            .trim()
            .into(),
        ),
        name: name!("stream"),
        arguments: vec![
            Node::new(InputValueDefinition {
                description: None,
                name: name!("label"),
                ty: ty!(String).into(),
                default_value: None,
                directives: Default::default(),
            }),
            Node::new(InputValueDefinition {
                description: None,
                name: name!("if"),
                ty: ty!(Boolean!).into(),
                default_value: Some(true.into()),
                directives: Default::default(),
            }),
            Node::new(InputValueDefinition {
                description: None,
                name: name!("initialCount"),
                ty: ty!(Int).into(),
                default_value: Some(0.into()),
                directives: Default::default(),
            }),
        ],
        repeatable: false,
        locations: vec![DirectiveLocation::Field],
    })
}
