//! Implements API schema generation.
use apollo_compiler::name;
use apollo_compiler::schema::DirectiveDefinition;
use apollo_compiler::schema::DirectiveLocation;
use apollo_compiler::schema::InputValueDefinition;
use apollo_compiler::ty;
use apollo_compiler::Node;

use crate::error::FederationError;
use crate::link::inaccessible_spec_definition::InaccessibleSpecDefinition;
use crate::schema::position;
use crate::schema::FederationSchema;
use crate::schema::ValidFederationSchema;

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
                let object = position.get(schema.schema())?.clone();
                object
                    .fields
                    .keys()
                    .map(|field_name| position.field(field_name.clone()))
                    .try_for_each(|child| child.remove(schema))?;
            }
            position::TypeDefinitionPosition::Interface(position) => {
                let interface = position.get(schema.schema())?.clone();
                interface
                    .fields
                    .keys()
                    .map(|field_name| position.field(field_name.clone()))
                    .try_for_each(|child| child.remove(schema))?;
            }
            position::TypeDefinitionPosition::InputObject(position) => {
                let input_object = position.get(schema.schema())?.clone();
                input_object
                    .fields
                    .keys()
                    .map(|field_name| position.field(field_name.clone()))
                    .try_for_each(|child| child.remove(schema))?;
            }
            position::TypeDefinitionPosition::Enum(position) => {
                let enum_ = position.get(schema.schema())?.clone();
                enum_
                    .values
                    .keys()
                    .map(|field_name| position.value(field_name.clone()))
                    .try_for_each(|child| child.remove(schema))?;
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
    schema: ValidFederationSchema,
    options: ApiSchemaOptions,
) -> Result<ValidFederationSchema, FederationError> {
    // Create a whole new federation schema that we can mutate.
    let mut api_schema = FederationSchema::new(schema.schema().clone().into_inner())?;

    // As we compute the API schema of a supergraph, we want to ignore explicit definitions of `@defer` and `@stream` because
    // those correspond to the merging of potential definitions from the subgraphs, but whether the supergraph API schema
    // supports defer or not is unrelated to whether subgraphs support it.
    if let Some(defer) = api_schema.get_directive_definition(&name!("defer")) {
        defer.remove(&mut api_schema)?;
    }
    if let Some(stream) = api_schema.get_directive_definition(&name!("stream")) {
        stream.remove(&mut api_schema)?;
    }

    if let Some(inaccessible_spec) = InaccessibleSpecDefinition::get_from_schema(&api_schema)? {
        inaccessible_spec.validate_inaccessible(&api_schema)?;
        inaccessible_spec.remove_inaccessible_elements(&mut api_schema)?;
    }

    remove_core_feature_elements(&mut api_schema)?;

    let mut schema = api_schema.into_inner();

    if options.include_defer {
        schema
            .directive_definitions
            .insert(name!("defer"), defer_definition());
    }

    if options.include_stream {
        schema
            .directive_definitions
            .insert(name!("stream"), stream_definition());
    }

    crate::compat::make_print_schema_compatible(&mut schema);

    ValidFederationSchema::new(schema.validate()?)
}

fn defer_definition() -> Node<DirectiveDefinition> {
    Node::new(DirectiveDefinition {
        description: None,
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
        locations: vec![
            DirectiveLocation::FragmentSpread,
            DirectiveLocation::InlineFragment,
        ],
    })
}

fn stream_definition() -> Node<DirectiveDefinition> {
    Node::new(DirectiveDefinition {
        description: None,
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
