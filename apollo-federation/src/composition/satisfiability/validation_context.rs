use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use itertools::Itertools;

use crate::bail;
use crate::error::FederationError;
use crate::link::join_spec_definition::JoinSpecDefinition;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::FieldDefinitionPosition;
use crate::validate_supergraph_for_query_planning;

pub(super) struct ValidationContext {
    supergraph_schema: ValidFederationSchema,
    join_spec: &'static JoinSpecDefinition,
    join_type_directive: Node<ast::DirectiveDefinition>,
    join_field_directive: Node<ast::DirectiveDefinition>,
    types_to_contexts: IndexMap<Name, IndexSet<String>>, // mapping from type name to context names
}

impl ValidationContext {
    pub(super) fn new(supergraph_schema: ValidFederationSchema) -> Result<Self, FederationError> {
        let (_, join_spec, context_spec) =
            validate_supergraph_for_query_planning(&supergraph_schema)?;
        let join_type_directive = join_spec
            .type_directive_definition(&supergraph_schema)?
            .clone();
        let join_field_directive = join_spec
            .field_directive_definition(&supergraph_schema)?
            .clone();

        let mut types_to_contexts = IndexMap::default();
        if let Some(context_spec) = context_spec {
            let context_applications =
                supergraph_schema.context_directive_applications_in_supergraph(context_spec)?;
            for app in context_applications {
                let app = app?;
                let mut type_names = vec![app.target().type_name().clone()];
                match app.target() {
                    CompositeTypeDefinitionPosition::Interface(interface_type) => {
                        type_names.extend(
                            supergraph_schema
                                .all_implementation_types(interface_type)?
                                .iter()
                                .map(|type_pos| type_pos.type_name())
                                .cloned(),
                        );
                    }
                    CompositeTypeDefinitionPosition::Union(union_type) => {
                        let union_def = union_type.get(supergraph_schema.schema())?;
                        type_names.extend(union_def.members.iter().map(|m| m.name.clone()));
                    }
                    _ => {}
                };
                for type_name in type_names {
                    types_to_contexts
                        .entry(type_name)
                        .or_insert_with(IndexSet::default)
                        .insert(app.arguments().name.to_string());
                }
            }
        }

        Ok(ValidationContext {
            supergraph_schema,
            join_spec,
            join_type_directive,
            join_field_directive,
            types_to_contexts,
        })
    }

    pub(super) fn is_shareable(
        &self,
        field: &FieldDefinitionPosition,
    ) -> Result<bool, FederationError> {
        let Ok(type_in_supergraph) = self
            .supergraph_schema
            .get_type(field.parent().type_name().clone())
        else {
            bail!("Type {} should exist in the supergraph", field.parent());
        };
        let Ok(type_in_supergraph) = CompositeTypeDefinitionPosition::try_from(type_in_supergraph)
        else {
            bail!("Type {} should be composite", field.parent().type_name());
        };
        if !type_in_supergraph.is_object_type() {
            return Ok(false);
        }

        let Ok(field_in_supergraph) = type_in_supergraph.field(field.field_name().clone()) else {
            bail!(
                "Field {} should exist in the supergraph",
                field.field_name()
            );
        };
        let join_field_apps = field_in_supergraph
            .get_applied_directives(&self.supergraph_schema, &self.join_field_directive.name);
        // A field is shareable if either:
        // 1) there is not join__field, but multiple join__type
        // 2) there is more than one join__field where the field is neither external nor overridden.
        if join_field_apps.is_empty() {
            let join_type_apps = type_in_supergraph
                .get_applied_directives(&self.supergraph_schema, &self.join_type_directive.name);
            Ok(join_type_apps.len() > 1)
        } else {
            let count = join_field_apps
                .iter()
                .map(|app| self.join_spec.field_directive_arguments(app))
                .process_results(|iter| {
                    iter.filter(|args| {
                        !(args.external.is_some_and(|x| x))
                            && !(args.user_overridden.is_some_and(|x| x))
                    })
                    .count()
                })?;
            Ok(count > 1)
        }
    }

    pub(super) fn matching_contexts(&self, type_name: &Name) -> Option<&IndexSet<String>> {
        self.types_to_contexts.get(type_name)
    }
}

#[cfg(test)]
mod validation_context_tests {
    use apollo_compiler::Name;

    use crate::composition::satisfiability::validation_context::ValidationContext;
    use crate::composition::satisfiability::*;
    use crate::schema::position::CompositeTypeDefinitionPosition;

    const TEST_SUPERGRAPH: &str = r#"
schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION)
  @link(url: "https://specs.apollo.dev/context/v0.1", for: SECURITY)
{
  query: Query
}

directive @context(name: String!) repeatable on INTERFACE | OBJECT | UNION

directive @context__fromContext(field: context__ContextFieldValue) on ARGUMENT_DEFINITION

directive @join__directive(graphs: [join__Graph!], name: String!, args: join__DirectiveArguments) repeatable on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION

directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String, contextArguments: [join__ContextArgument!]) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

directive @join__graph(name: String!, url: String!) on ENUM_VALUE

directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

scalar context__ContextFieldValue

interface I
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
  @context(name: "A__contextI")
{
  id: ID!
  value: Int! @join__field(graph: A)
}

input join__ContextArgument {
  name: String!
  type: String!
  context: String!
  selection: join__FieldValue!
}

scalar join__DirectiveArguments

scalar join__FieldSet

scalar join__FieldValue

enum join__Graph {
  A @join__graph(name: "A", url: "http://A")
  B @join__graph(name: "B", url: "http://B")
}

scalar link__Import

enum link__Purpose {
  """
  `SECURITY` features provide metadata necessary to securely resolve fields.
  """
  SECURITY

  """
  `EXECUTION` features provide metadata necessary for operation execution.
  """
  EXECUTION
}

type P
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
{
  id: ID!
  data: String! @join__field(graph: A, contextArguments: [{context: "A__contextI", name: "onlyInA", type: "Int", selection: " { value }"}])
}

type Query
  @join__type(graph: A)
  @join__type(graph: B)
{
  start: I! @join__field(graph: B)
}

type T implements I
  @join__implements(graph: A, interface: "I")
  @join__implements(graph: B, interface: "I")
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
{
  id: ID!
  value: Int! @join__field(graph: A)
  onlyInA: Int! @join__field(graph: A)
  p: P! @join__field(graph: A)
  sharedField: Int!
  onlyInB: Int! @join__field(graph: B)
}
    "#;

    fn is_shareable_field(context: &ValidationContext, type_name: &str, field_name: &str) -> bool {
        let supergraph_schema = &context.supergraph_schema;
        let type_pos = supergraph_schema
            .get_type(Name::new_unchecked(type_name))
            .unwrap();
        let type_pos = CompositeTypeDefinitionPosition::try_from(type_pos).unwrap();
        let field_pos = type_pos.field(Name::new_unchecked(field_name)).unwrap();
        context.is_shareable(&field_pos).unwrap()
    }

    #[test]
    fn test_is_shareable() {
        let supergraph = Supergraph::parse(TEST_SUPERGRAPH).unwrap();
        let supergraph_schema = supergraph.schema().clone();
        let context = ValidationContext::new(supergraph_schema).unwrap();

        assert!(is_shareable_field(&context, "P", "id"));
        assert!(!is_shareable_field(&context, "P", "data"));
        assert!(is_shareable_field(&context, "T", "sharedField"));
        assert!(!is_shareable_field(&context, "T", "onlyInB"));
    }

    fn matching_contexts<'a>(
        context: &'a ValidationContext,
        type_name: &str,
    ) -> Option<Vec<&'a str>> {
        context
            .matching_contexts(&Name::new_unchecked(type_name))
            .map(|set| set.iter().map(|s| s.as_str()).collect())
    }

    #[test]
    fn test_matching_contexts() {
        let supergraph = Supergraph::parse(TEST_SUPERGRAPH).unwrap();
        let supergraph_schema = supergraph.schema().clone();
        let context = ValidationContext::new(supergraph_schema).unwrap();

        assert_eq!(matching_contexts(&context, "I"), Some(vec!["A__contextI"]),);
        assert_eq!(matching_contexts(&context, "T"), Some(vec!["A__contextI"]),);
        assert_eq!(matching_contexts(&context, "P"), None,);
    }
}
