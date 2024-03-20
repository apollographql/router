use anyhow::anyhow;
use apollo_compiler::executable::ExecutableDocument;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::FragmentSpread;
use apollo_compiler::executable::InlineFragment;
use apollo_compiler::executable::Operation;
use apollo_compiler::executable::Selection;
use apollo_compiler::Schema;
use tower::BoxError;

use super::directives::IncludeDirective;
use super::directives::SkipDirective;
use super::CostCalculator;

pub(crate) struct BasicCostCalculator {}

impl BasicCostCalculator {
    fn score_field(field: &Field, schema: &Schema) -> Result<f64, BoxError> {
        if BasicCostCalculator::skipped_by_directives(field) {
            return Ok(0.0);
        }

        let ty = field
            .inner_type_def(schema)
            .ok_or(anyhow!("Field {} was not found in schema", field))?;

        let instance_count = if field.ty().is_list() { 100.0 } else { 1.0 };

        let mut type_cost = if ty.is_interface() || ty.is_object() || ty.is_union() {
            1.0
        } else {
            0.0
        };
        for selection in field.selection_set.selections.iter() {
            type_cost += BasicCostCalculator::score_selection(selection, schema)?;
        }

        Ok(instance_count * type_cost)
    }

    fn score_fragment_spread(_fragment_spread: &FragmentSpread) -> Result<f64, BoxError> {
        Ok(0.0)
    }

    fn score_inline_fragment(
        inline_fragment: &InlineFragment,
        schema: &Schema,
    ) -> Result<f64, BoxError> {
        let mut cost = 0.0;
        for selection in inline_fragment.selection_set.selections.iter() {
            cost += BasicCostCalculator::score_selection(selection, schema)?;
        }
        Ok(cost)
    }

    fn score_operation(operation: &Operation, schema: &Schema) -> Result<f64, BoxError> {
        let mut cost = 0.0;
        if operation.is_mutation() {
            cost += 10.0;
        }

        for selection in operation.selection_set.selections.iter() {
            cost += BasicCostCalculator::score_selection(selection, schema)?;
        }

        Ok(cost)
    }

    fn score_selection(selection: &Selection, schema: &Schema) -> Result<f64, BoxError> {
        match selection {
            Selection::Field(f) => BasicCostCalculator::score_field(f, schema),
            Selection::FragmentSpread(s) => BasicCostCalculator::score_fragment_spread(s),
            Selection::InlineFragment(i) => BasicCostCalculator::score_inline_fragment(i, schema),
        }
    }

    fn skipped_by_directives(field: &Field) -> bool {
        let include_directive = IncludeDirective::from_field(field);
        if let Ok(Some(IncludeDirective { is_included: false })) = include_directive {
            return true;
        }

        let skip_directive = SkipDirective::from_field(field);
        if let Ok(Some(SkipDirective { is_skipped: true })) = skip_directive {
            return true;
        }

        false
    }
}

impl CostCalculator for BasicCostCalculator {
    fn estimated(query: &ExecutableDocument, schema: &Schema) -> Result<f64, BoxError> {
        let mut cost = 0.0;
        if let Some(op) = &query.anonymous_operation {
            cost += BasicCostCalculator::score_operation(op, schema)?;
        }
        for (_name, op) in query.named_operations.iter() {
            cost += BasicCostCalculator::score_operation(op, schema)?;
        }
        Ok(cost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cost(schema_str: &str, query_str: &str) -> f64 {
        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let query = ExecutableDocument::parse_and_validate(&schema, query_str, "").unwrap();
        BasicCostCalculator::estimated(&query, &schema).unwrap()
    }

    #[test]
    fn query_cost() {
        let schema = "
            type Query {
                a(id: ID): String
                b: Int
            }
        ";
        let query = "
            {
                a(id: 2)
            }
        ";

        assert_eq!(cost(schema, query), 0.0)
    }

    #[test]
    fn mutation_cost() {
        let schema = "
            type Query {
                a: Int
            }
            type Mutation {
                doSomething: Int
            }
        ";
        let query = "
            mutation {
                doSomething
            }
        ";

        assert_eq!(cost(schema, query), 10.0)
    }

    #[test]
    fn object_cost() {
        let schema = "
            type Query {
                me: User!
            }

            type User {
                name: String!
                age: Int
            }
        ";
        let query = "
            {
                me {
                    name
                }
            }
        ";

        assert_eq!(cost(schema, query), 1.0)
    }

    #[test]
    fn interface_cost() {
        let schema = "
            type Query {
                favoriteBook: Book
            }

            interface Book {
                title: String!
                author: String!
            }
        ";
        let query = "
            {
                favoriteBook {
                    title
                }
            }
        ";

        assert_eq!(cost(schema, query), 1.0)
    }

    #[test]
    fn union_cost() {
        let schema = "
            type Query {
                fruit: Fruit!
            }

            type Apple {
                weight: Float
            }

            type Orange {
                weight: Float
            }

            union Fruit = Apple | Orange
        ";
        let query = "
            {
                fruit {
                    ... on Apple {
                        weight
                    }
                    ... on Orange {
                        weight
                    }
                }
            }
        ";

        assert_eq!(cost(schema, query), 1.0)
    }

    #[test]
    fn list_cost() {
        let schema = "
            type Query {
                products: [Product!]
            }

            type Product {
                name: String
                cost: Float
            }
        ";
        let query = "
            {
                products {
                    name
                    cost
                }
            }
        ";

        assert_eq!(cost(schema, query), 100.0)
    }

    #[test]
    fn nested_object_lists() {
        let schema = "
            type Query {
                authors: [Author]
                books: [Book]
            }

            type Author {
                books: [Book]
                name: String
            }

            type Book {
                authors: [Author]
                title: String
            }
        ";
        let query = "
            {
                authors {
                    books {
                        title
                    }
                }
            }
        ";

        // The scoring works recursively starting at the leaf nodes of the query.
        //
        // The leaf selection is a Book object, which has cost 1.
        //
        // The parent is itself a selection of an Author object, which has an overhead of 1, plus
        // the cost of its children (assumed to be a list of 100 books). So the cost of each author
        // is 101.
        //
        // The query selects a list of authors, which is also assumed to have 100 items. So the cost
        // of the query overall is 101 * 100, or 10,100.
        assert_eq!(cost(schema, query), 10100.0)
    }

    #[test]
    fn skip_directive_excludes_cost() {
        let schema = "
            type Query {
                authors: [Author]
            }

            type Author {
                books: [Book]
                name: String
            }

            type Book {
                title: String
            }
        ";
        let query = "
            {
                authors {
                    books @skip(if: true) {
                        title
                    }
                    name
                }
            }
        ";

        assert_eq!(cost(schema, query), 100.0)
    }

    #[test]
    fn include_directive_excludes_cost() {
        let schema = "
            type Query {
                authors: [Author]
            }

            type Author {
                books: [Book]
                name: String
            }

            type Book {
                title: String
            }
        ";
        let query = "
            {
                authors {
                    books @include(if: false) {
                        title
                    }
                    name
                }
            }
        ";

        assert_eq!(cost(schema, query), 100.0)
    }
}
