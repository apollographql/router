use crate::connectors::{spec::source::FRAGMENTS_NAME_ARGUMENT, string_template::Expression, validation::{Code, coordinates::{FragmentsCoordinate, SourceDirectiveCoordinate}, errors::ErrorsCoordinate, expression::{self, Context, MappingArgument, parse_mapping_argument}, graphql::SchemaInfo}};

use apollo_compiler::{Name, Node};
use apollo_compiler::ast::Value;
use indexmap::IndexMap;
use shape::Shape;
use crate::connectors::validation::Message;


/// The`@source(fragments:)` argument
pub(crate) struct FragmentsArgument<'schema> {
    coordinate: FragmentsCoordinate<'schema>,
    node: Node<Value>,
    object: IndexMap<Name, (Expression, Node<Value>)>
}

impl<'schema> FragmentsArgument<'schema> {
    pub(crate) fn parse_for_source(
        source: SourceDirectiveCoordinate<'schema>,
        schema: &'schema SchemaInfo,
    ) -> Result<Option<Self>, Message> {
        Self::parse(
            FragmentsCoordinate {
                coordinate: ErrorsCoordinate::Source { source },
            },
            schema,
        )
    }

    fn parse(
        coordinate: FragmentsCoordinate<'schema>,
        schema: &'schema SchemaInfo,
    ) -> Result<Option<Self>, Message> {
        let directive = match &coordinate.coordinate {
            ErrorsCoordinate::Source { source } => source.directive,
            ErrorsCoordinate::Connect { connect } => connect.directive,
        };
        // If the `isSuccess` argument cannot be found in provided args, Error
        let Some(value) = directive.specified_argument_by_name(&FRAGMENTS_NAME_ARGUMENT) else {
            return Ok(None);
        };
        
        let Some(obj) = value.as_object() else {
            return Err(Message{
                code: Code::InvalidFragments,
                message: "fragments are required to be objects".to_string(),
                locations: Vec::new(),
            })
        };

        let object = obj
            .iter()
            .map(|(key, node)| {
                let MappingArgument { expression, node } =
                    parse_mapping_argument(node, coordinate.clone(), Code::InvalidFragments, schema)?;
                Ok((
                    key.clone(),
                    (expression, node)
            ))})
            .collect::<Result<IndexMap<_, _>, Message>>()?;

        Ok(Some(Self {
            object,            
            coordinate,
            node: value.to_owned()
        }))
    }

    /// Check that only available variables are used, and the expression results in a boolean
    pub(crate) fn type_check(self, schema: &SchemaInfo<'_>) -> Result<(), Message> {
        let context = match self.coordinate.coordinate {
            ErrorsCoordinate::Source { .. } => {
                &Context::for_source_response(schema, &self.node, Code::InvalidFragments)
            }
            ErrorsCoordinate::Connect { .. } => unreachable!(),
        };
        for (_fragment, (expression, _node)) in self.object.iter() {
            expression::validate(expression, context, &Shape::none()).map_err(|mut message| {
                message.message = format!(
                    "In {coordinate}: {message}",
                    coordinate = self.coordinate,
                    message = message.message
                );
                message
            })?;
        }
        Ok(())
    }
}