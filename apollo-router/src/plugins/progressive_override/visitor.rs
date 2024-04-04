//! Progressive override operation/schema traversal
use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::ast;
use apollo_compiler::executable;
use apollo_compiler::schema;
use tower::BoxError;

use super::JOIN_FIELD_DIRECTIVE_NAME;
use super::JOIN_SPEC_BASE_URL;
use super::JOIN_SPEC_VERSION_RANGE;
use super::OVERRIDE_LABEL_ARG_NAME;
use crate::spec::query::traverse;
use crate::spec::Schema;

impl<'a> OverrideLabelVisitor<'a> {
    pub(crate) fn new(schema: &'a schema::Schema) -> Option<Self> {
        Some(Self {
            schema,
            override_labels: HashSet::new(),
            join_field_directive_name: Schema::directive_name(
                schema,
                JOIN_SPEC_BASE_URL,
                JOIN_SPEC_VERSION_RANGE,
                JOIN_FIELD_DIRECTIVE_NAME,
            )?,
        })
    }
}

impl<'a> traverse::Visitor for OverrideLabelVisitor<'a> {
    fn schema(&self) -> &apollo_compiler::Schema {
        self.schema
    }

    fn operation(&mut self, root_type: &str, node: &executable::Operation) -> Result<(), BoxError> {
        traverse::operation(self, root_type, node)
    }

    fn field(
        &mut self,
        _parent_type: &str,
        field_def: &ast::FieldDefinition,
        node: &executable::Field,
    ) -> Result<(), BoxError> {
        let new_override_labels = field_def
            .directives
            .iter()
            .filter_map(|d| {
                if d.name.as_str() == self.join_field_directive_name {
                    Some(d.arguments.iter().filter_map(|arg| {
                        if arg.name == OVERRIDE_LABEL_ARG_NAME {
                            arg.value.as_str().map(|s| Arc::new(s.to_string()))
                        } else {
                            None
                        }
                    }))
                } else {
                    None
                }
            })
            .flatten();
        self.override_labels.extend(new_override_labels);

        traverse::field(self, field_def, node)
    }
}

pub(crate) struct OverrideLabelVisitor<'a> {
    schema: &'a schema::Schema,
    pub(crate) override_labels: HashSet<Arc<String>>,
    join_field_directive_name: String,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apollo_compiler::validation::Valid;
    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;

    use crate::plugins::progressive_override::visitor::OverrideLabelVisitor;
    use crate::spec::query::traverse;

    const SCHEMA: &str = r#"
      schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/join/v0.4", for: EXECUTION)
      {
        query: Query
      }

      directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

      directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean, overrideLabel: String) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

      directive @join__graph(name: String!, url: String!) on ENUM_VALUE

      directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

      directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

      directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

      directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

      scalar join__FieldSet

      enum join__Graph {
        SUBGRAPH1 @join__graph(name: "Subgraph1", url: "https://Subgraph1")
        SUBGRAPH2 @join__graph(name: "Subgraph2", url: "https://Subgraph2")
      }

      scalar link__Import

      enum link__Purpose {
        """
        \`SECURITY\` features provide metadata necessary to securely resolve fields.
        """
        SECURITY

        """
        \`EXECUTION\` features provide metadata necessary for operation execution.
        """
        EXECUTION
      }

      type Query
        @join__type(graph: SUBGRAPH1)
        @join__type(graph: SUBGRAPH2)
      {
        t: T @join__field(graph: SUBGRAPH1)
        t2: T @join__field(graph: SUBGRAPH1, override: "Subgraph2", overrideLabel: "foo2") @join__field(graph: SUBGRAPH2, overrideLabel: "foo2")
      }

      type T
        @join__type(graph: SUBGRAPH1, key: "k")
        @join__type(graph: SUBGRAPH2, key: "k")
      {
        k: ID
        a: Int @join__field(graph: SUBGRAPH1, override: "Subgraph2", overrideLabel: "foo") @join__field(graph: SUBGRAPH2, overrideLabel: "foo")
        b: Int @join__field(graph: SUBGRAPH2)
      }
    "#;

    #[test]
    fn collects() {
        let schema = Schema::parse(SCHEMA, "supergraph.graphql").expect("parse schema");
        let operation_string = "{ t { k a b } }";
        let operation = ExecutableDocument::parse(
            Valid::assume_valid_ref(&schema),
            operation_string,
            "query.graphql",
        )
        .expect("parse operation");

        let mut visitor = OverrideLabelVisitor::new(&schema).expect("create visitor");

        traverse::document(&mut visitor, &operation, None).unwrap();

        assert_eq!(
            visitor.override_labels,
            vec![Arc::new("foo".to_string())].into_iter().collect()
        );
    }

    #[test]
    fn collects2() {
        let schema = Schema::parse(SCHEMA, "supergraph.graphql").expect("parse schema");
        let operation_string = "{ t { k a b } t2 }";
        let operation = ExecutableDocument::parse(
            Valid::assume_valid_ref(&schema),
            operation_string,
            "query.graphql",
        )
        .expect("parse operation");

        let mut visitor = OverrideLabelVisitor::new(&schema).expect("create visitor");

        traverse::document(&mut visitor, &operation, None).unwrap();

        assert_eq!(
            visitor.override_labels,
            vec![Arc::new("foo".to_string()), Arc::new("foo2".to_string())]
                .into_iter()
                .collect()
        );
    }
}
