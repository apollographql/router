use anyhow::anyhow;
use apollo_compiler::ast;
use apollo_compiler::validation::Valid;
use apollo_compiler::Schema;
use tower::BoxError;

use super::cost_directive::CostDirective;
use super::list_size_directive::ListSizeDirective;

use crate::spec::query::traverse;

pub(crate) struct CostAnalyzer<'a> {
    supergraph_schema: &'a Valid<Schema>,
    cost: f64,
}

impl<'a> CostAnalyzer<'a> {
    pub(crate) fn new(supergraph_schema: &'a Valid<Schema>) -> Self {
        Self {
            supergraph_schema,
            cost: 0.0,
        }
    }

    pub(crate) fn estimate(&mut self, query: &ast::Document) -> Result<f64, BoxError> {
        self.cost = 0.0;
        traverse::document(self, query)?;
        Ok(self.cost)
    }

    fn traverse_list_field(
        &mut self,
        field_def: &ast::FieldDefinition,
        field: &ast::Field,
    ) -> Result<(), BoxError> {
        let directive = ListSizeDirective::from_field(field_def)?;
        let max_size = directive.max_list_size()?;

        let mut subtree_analyzer = CostAnalyzer::new(self.supergraph_schema);
        traverse::field(&mut subtree_analyzer, field_def, field)?;

        self.cost += max_size * subtree_analyzer.cost;

        Ok(())
    }

    fn traverse_individual_field(
        &mut self,
        field_def: &ast::FieldDefinition,
        field: &ast::Field,
    ) -> Result<(), BoxError> {
        let ty = self
            .supergraph_schema
            .types
            .get(field_def.ty.inner_named_type())
            .ok_or(anyhow!("Field definition not recognized in schema"))?;
        let default_cost = if ty.is_interface() || ty.is_object() {
            1.0
        } else {
            0.0
        };

        let directive = CostDirective::from_field(field_def)?;
        if let Some(cost) = directive {
            self.cost += cost.weight();
        } else {
            self.cost += default_cost;
        }

        traverse::field(self, field_def, field)
    }
}

impl<'a> traverse::Visitor for CostAnalyzer<'a> {
    fn schema(&self) -> &apollo_compiler::Schema {
        self.supergraph_schema
    }

    fn operation(
        &mut self,
        root_type: &str,
        def: &ast::OperationDefinition,
    ) -> Result<(), BoxError> {
        match def.operation_type {
            ast::OperationType::Mutation => {
                self.cost += 10.0;
            }
            ast::OperationType::Query => {
                self.cost += 1.0;
            }
            ast::OperationType::Subscription => {
                // no-op
            }
        }

        traverse::operation(self, root_type, def)
    }

    fn field(
        &mut self,
        _parent_type: &str,
        field_def: &ast::FieldDefinition,
        field: &ast::Field,
    ) -> Result<(), BoxError> {
        let cost_directive = CostDirective::from_field(field_def)?;
        if let Some(cost_directive) = cost_directive {
            self.cost += cost_directive.weight();
            return Ok(());
        }

        if field_def.ty.is_list() {
            self.traverse_list_field(field_def, field)?;
        } else {
            self.traverse_individual_field(field_def, field)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_query_cost() {
        let schema_str = "
            type Query {
                a(id: ID): String
                b: Int
            }
        ";
        let query_str = "
            {
                a(id: 2)
            }
        ";

        let schema = apollo_compiler::Schema::parse_and_validate(schema_str, "").unwrap();
        let query = ast::Document::parse(query_str, "").unwrap();

        let mut analyzer = CostAnalyzer::new(&schema);
        let cost = analyzer.estimate(&query).unwrap();

        assert_eq!(cost, 1.0)
    }

    #[test]
    fn default_mutation_cost() {
        let schema_str = "
            type Query {
                a: Int
            }

            type Mutation {
                doSomething: Int
            }
        ";
        let query_str = "
            mutation {
                doSomething
            }
        ";

        let schema = apollo_compiler::Schema::parse_and_validate(schema_str, "").unwrap();
        let query = ast::Document::parse(query_str, "").unwrap();

        let mut analyzer = CostAnalyzer::new(&schema);
        let cost = analyzer.estimate(&query).unwrap();

        assert_eq!(cost, 10.0)
    }

    #[test]
    fn custom_cost() {
        let schema_str = r#"
            directive @cost(weight: String!) on 
                | ARGUMENT_DEFINITION
                | ENUM
                | FIELD_DEFINITION
                | INPUT_FIELD_DEFINITION
                | OBJECT
                | SCALAR

            type Query {
                a: Int @cost(weight: "25")
            }
        "#;
        let query_str = "{ a }";

        let schema = apollo_compiler::Schema::parse_and_validate(schema_str, "").unwrap();
        let query = ast::Document::parse(query_str, "").unwrap();

        let mut analyzer = CostAnalyzer::new(&schema);
        let cost = analyzer.estimate(&query).unwrap();

        assert_eq!(cost, 26.0)
    }

    #[test]
    fn custom_cost_inside_list() {
        // https://ibm.github.io/graphql-specs/cost-spec.html#example-c3975
        let schema_str = r#"
            directive @cost(weight: String!) on 
                | ARGUMENT_DEFINITION
                | ENUM
                | FIELD_DEFINITION
                | INPUT_FIELD_DEFINITION
                | OBJECT
                | SCALAR

            directive @listSize(
                assumedSize: Int,
                slicingArguments: [String!],
                sizedFields: [String!],
                requireOneSlicingArgument: Boolean = true
                ) on FIELD_DEFINITION

            type User {
                name: String
                age: Int @cost(weight: "2.0")
            }

            type Query {
                users: [User] @listSize(assumedSize: 5)
            }
        "#;
        let query_str = "
            query Example {
                users {
                    age
                }
            }
        ";

        let schema = apollo_compiler::Schema::parse_and_validate(schema_str, "").unwrap();
        let query = ast::Document::parse(query_str, "").unwrap();

        let mut analyzer = CostAnalyzer::new(&schema);
        let cost = analyzer.estimate(&query).unwrap();

        assert_eq!(analyzer.cost, 11.0)
    }

    #[ignore = "slicingArguments is not yet implemented"]
    #[test]
    fn ibm_spec_example_1() {
        // https://ibm.github.io/graphql-specs/cost-spec.html#example-c3975
        let schema_str = r#"
            directive @cost(weight: String!) on 
                | ARGUMENT_DEFINITION
                | ENUM
                | FIELD_DEFINITION
                | INPUT_FIELD_DEFINITION
                | OBJECT
                | SCALAR

            directive @listSize(
                assumedSize: Int,
                slicingArguments: [String!],
                sizedFields: [String!],
                requireOneSlicingArgument: Boolean = true
                ) on FIELD_DEFINITION

            type User {
                name: String
                age: Int @cost(weight: "2.0")
            }

            type Query {
                users(max: Int): [User] @listSize(slicingArguments: ["max"])
            }
        "#;
        // https://ibm.github.io/graphql-specs/cost-spec.html#example-e5fe6
        let query_str = "
            query Example {
                users (max: 5) {
                    age
                }
            }
        ";

        let schema = apollo_compiler::Schema::parse_and_validate(schema_str, "").unwrap();
        let query = ast::Document::parse(query_str, "").unwrap();

        let mut analyzer = CostAnalyzer::new(&schema);
        let cost = analyzer.estimate(&query).unwrap();

        assert_eq!(cost, 11.0)
    }

    #[test]
    fn ibm_spec_example_10() {
        // https://ibm.github.io/graphql-specs/cost-spec.html#example-680a6
        let schema_str = r#"
            directive @cost(weight: String!) on 
                | ARGUMENT_DEFINITION
                | ENUM
                | FIELD_DEFINITION
                | INPUT_FIELD_DEFINITION
                | OBJECT
                | SCALAR

            directive @listSize(
                assumedSize: Int,
                slicingArguments: [String!],
                sizedFields: [String!],
                requireOneSlicingArgument: Boolean = true
                ) on FIELD_DEFINITION

            input Filter {
                f: String
            }

            type Query {
                topProducts(filter: Filter @cost(weight: "15.0")): [String] @cost(weight: "5.0") @listSize(assumedSize: 10)
            }
        "#;
        // https://ibm.github.io/graphql-specs/cost-spec.html#example-e5fe6
        let query_str = "
            query Example {
                topProducts
            }
        ";

        let schema = apollo_compiler::Schema::parse_and_validate(schema_str, "").unwrap();
        let query = ast::Document::parse(query_str, "").unwrap();

        let mut analyzer = CostAnalyzer::new(&schema);
        let cost = analyzer.estimate(&query).unwrap();

        assert_eq!(cost, 6.0)
    }
}
