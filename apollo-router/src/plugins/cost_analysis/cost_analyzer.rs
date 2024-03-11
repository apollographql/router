use apollo_compiler::ast;
use apollo_compiler::validation::Valid;
use apollo_compiler::schema;
use apollo_compiler::Schema;
use tower::BoxError;

use super::cost_directive::CostDirective;
use super::list_size_directive::ListSizeDirective;

use crate::spec::query::traverse;

pub(crate) struct CostAnalyzer<'a> {
    supergraph_schema: &'a Valid<Schema>,
    query: &'a ast::Document,
    cost: f64,
}

impl<'a> CostAnalyzer<'a> {
    pub(crate) fn new(supergraph_schema: &'a Valid<Schema>, query: &'a ast::Document) -> Self {
        Self { supergraph_schema, query, cost: 0.0 }
    }

    pub(crate) fn get_cost(&mut self) -> Result<(), BoxError> {
        traverse::document(self, self.query)
    }

    fn record_list_cost(&mut self, field: &ast::FieldDefinition) -> Result<(), BoxError> {
        let directive = ListSizeDirective::from_field(field)?;
        let max_size = directive.max_list_size()?;

        self.cost += max_size;

        Ok(())
    }

    fn record_individual_field_cost(&mut self, field: &ast::FieldDefinition, ty: &schema::ExtendedType) -> Result<(), BoxError> {
        let default_cost = if ty.is_interface() || ty.is_object() {
            1.0
        } else {
            0.0
        };

        let directive = CostDirective::from_field(field)?;
        if let Some(cost) = directive {
            self.cost += cost.weight();
        } else {
            self.cost += default_cost;
        }

        Ok(())
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
            },
            ast::OperationType::Query => {
                self.cost += 1.0;
            },
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
        def: &ast::Field,
    ) -> Result<(), BoxError> {
        if let Some(ty) = self.supergraph_schema.types.get(field_def.ty.inner_named_type()) {
            if field_def.ty.is_list() {
                self.record_list_cost(field_def)?;
            } else {
                self.record_individual_field_cost(field_def, ty)?;
            }
        }

        traverse::field(self, field_def, def)
    }
}

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
    let mut analyzer = CostAnalyzer::new(&schema, &query);

    traverse::document(&mut analyzer, &query).unwrap();
    assert_eq!(analyzer.cost, 1.0)
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
    let mut analyzer = CostAnalyzer::new(&schema, &query);

    traverse::document(&mut analyzer, &query).unwrap();
    assert_eq!(analyzer.cost, 10.0)
}

#[test]
fn list_query_cost() {
    let schema_str = "
        directive @listSize(
            assumedSize: Int,
            slicingArguments: [String!],
            sizedFields: [String!],
            requireOneSlicingArgument: Boolean = true
            ) on FIELD_DEFINITION

        type Query {
            a: Int
            b: [Int] @listSize(assumedSize: 10)
        }
    ";
    let query_str = "
        {
            a
            b
        }
    ";

    let schema = apollo_compiler::Schema::parse_and_validate(schema_str, "").unwrap();
    let query = ast::Document::parse(query_str, "").unwrap();
    let mut analyzer = CostAnalyzer::new(&schema, &query);

    traverse::document(&mut analyzer, &query).unwrap();
    assert_eq!(analyzer.cost, 11.0)
}

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
    let mut analyzer = CostAnalyzer::new(&schema, &query);

    traverse::document(&mut analyzer, &query).unwrap();
    assert_eq!(analyzer.cost, 11.0)
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
            topProducts(filter: {})
        }
    ";

    let schema = apollo_compiler::Schema::parse_and_validate(schema_str, "").unwrap();
    let query = ast::Document::parse(query_str, "").unwrap();
    let mut analyzer = CostAnalyzer::new(&schema, &query);

    traverse::document(&mut analyzer, &query).unwrap();
    assert_eq!(analyzer.cost, 5.0)
}