//! Variable validation.
mod resolver;

use std::collections::HashMap;

use itertools::Itertools;
use resolver::args::ArgsResolver;
use resolver::this::ThisResolver;
use resolver::NamespaceResolver;

use crate::sources::connect::validation::graphql::GraphQLString;
use crate::sources::connect::validation::graphql::SchemaInfo;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::variable::Namespace;
use crate::sources::connect::variable::Target;
use crate::sources::connect::variable::VariableContext;
use crate::sources::connect::variable::VariableReference;

pub(crate) struct VariableResolver<'a> {
    context: VariableContext<'a>,
    schema: &'a SchemaInfo<'a>,
    resolvers: HashMap<Namespace, Box<dyn NamespaceResolver + 'a>>,
}

impl<'a> VariableResolver<'a> {
    pub(super) fn new(context: VariableContext<'a>, schema: &'a SchemaInfo<'a>) -> Self {
        let mut resolvers = HashMap::<Namespace, Box<dyn NamespaceResolver + 'a>>::new();
        resolvers.insert(
            Namespace::This,
            Box::new(ThisResolver::new(context.object, context.field)),
        );
        resolvers.insert(Namespace::Args, Box::new(ArgsResolver::new(context.field)));
        Self {
            context,
            schema,
            resolvers,
        }
    }

    pub(super) fn resolve(
        &self,
        reference: &VariableReference<Namespace>,
        expression: GraphQLString,
    ) -> Result<(), Message> {
        if !self
            .context
            .available_namespaces()
            .contains(&reference.namespace.namespace)
        {
            return Err(Message {
                code: self.error_code(),
                message: format!(
                    "variable `{namespace}` is not valid at this location, must be one of {available}",
                    namespace = reference.namespace.namespace.as_str(),
                    available = self.context.namespaces_joined(),
                ),
                locations: expression.line_col_for_subslice(
                    reference.namespace.location.start..reference.namespace.location.end,
                    self.schema
                ).into_iter().collect(),
            });
        }
        if let Some(resolver) = self.resolvers.get(&reference.namespace.namespace) {
            resolver.check(reference, expression, self.schema)?;
        }
        Ok(())
    }

    fn error_code(&self) -> Code {
        match self.context.target {
            Target::Url => Code::InvalidUrl,
            Target::Header => Code::InvalidHeader,
            Target::Body => Code::InvalidJsonSelection,
        }
    }
}
