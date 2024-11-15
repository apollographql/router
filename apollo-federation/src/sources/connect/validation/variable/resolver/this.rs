use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::Type;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::Node;

use crate::sources::connect::validation::graphql::GraphQLString;
use crate::sources::connect::validation::graphql::SchemaInfo;
use crate::sources::connect::validation::variable::resolver;
use crate::sources::connect::validation::variable::resolver::NamespaceResolver;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::variable::Namespace;
use crate::sources::connect::variable::VariableReference;

/// Resolves variables in the `$this` namespace
pub(crate) struct ThisResolver<'a> {
    object: &'a Node<ObjectType>,
    field: &'a Component<FieldDefinition>,
}

impl<'a> ThisResolver<'a> {
    pub(crate) fn new(object: &'a Node<ObjectType>, field: &'a Component<FieldDefinition>) -> Self {
        Self { object, field }
    }
}

impl<'a> NamespaceResolver for ThisResolver<'a> {
    fn resolve(
        &self,
        reference: &VariableReference<Namespace>,
        expression: GraphQLString,
        schema: &SchemaInfo,
    ) -> Result<Option<Type>, Message> {
        let root = resolver::get_root(reference, expression, schema)?;
        let field_type = self
            .object
            .fields
            .get(root.as_str())
            .ok_or_else(|| Message {
                code: Code::UndefinedField,
                message: format!(
                    "`{object}` does not have a field named `{root}`",
                    object = self.object.name,
                    root = root.as_str(),
                ),
                locations: expression
                    .line_col_for_subslice(root.location.start..root.location.end, schema)
                    .into_iter()
                    .collect(),
            })
            .map(|field| field.ty.clone())?;

        resolver::resolve_path(schema, reference, expression, &field_type, self.field)
    }
}
