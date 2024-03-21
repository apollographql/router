use anyhow::anyhow;
use apollo_compiler::executable::ExecutableDocument;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::FragmentSpread;
use apollo_compiler::executable::InlineFragment;
use apollo_compiler::executable::Operation;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::validation::Valid;
use apollo_compiler::Schema;
use tower::BoxError;

use super::directives::IncludeDirective;
use super::directives::RequiresDirective;
use super::directives::SkipDirective;
use super::CostCalculator;

pub(crate) struct BasicCostCalculator {}

impl BasicCostCalculator {
    /// Scores a field within a GraphQL operation, handling some expected cases where
    /// directives change how the query is fetched. In the case of the federation
    /// directive `@requires`, the cost of the required selection is added to the
    /// cost of the current field. There's a chance this double-counts the cost of
    /// a selection if two fields require the same thing, or if a field is selected
    /// along with a field that it requires.
    ///
    /// ```graphql
    /// type Query {
    ///     foo: Foo @external
    ///     bar: Bar @requires(fields: "foo")
    ///     baz: Baz @requires(fields: "foo")
    /// }
    /// ```
    ///
    /// This should be okay, as we don't want this implementation to have to know about
    /// any deduplication happening in the query planner, and we're estimating an upper
    /// bound for cost anyway.
    fn score_field(field: &Field, schema: &Valid<Schema>) -> Result<f64, BoxError> {
        if BasicCostCalculator::skipped_by_directives(field) {
            return Ok(0.0);
        }

        let ty = field
            .inner_type_def(schema)
            .ok_or(anyhow!("Field {} was not found in schema", field))?;

        // Determine how many instances we're scoring. If there's no user-provided
        // information, assume lists have 100 items.
        let instance_count = if field.ty().is_list() { 100.0 } else { 1.0 };

        // Determine the cost for this particular field. Scalars are free, non-scalars are not.
        // For fields with selections, add in the cost of the selections as well.
        let mut type_cost = if ty.is_interface() || ty.is_object() || ty.is_union() {
            1.0
        } else {
            0.0
        };
        type_cost += BasicCostCalculator::score_selection_set(&field.selection_set, schema)?;

        // If the field is marked with `@requires`, the required selection may not be included
        // in the query's selection. Adding that requirement's cost to the field ensures it's
        // accounted for.
        let requirements = RequiresDirective::from_field(field, schema)?.map(|d| d.fields);
        let requirements_cost = match requirements {
            Some(selection_set) => {
                BasicCostCalculator::score_selection_set(&selection_set, schema)?
            }
            None => 0.0,
        };

        Ok(instance_count * type_cost + requirements_cost)
    }

    fn score_fragment_spread(_fragment_spread: &FragmentSpread) -> Result<f64, BoxError> {
        Ok(0.0)
    }

    fn score_inline_fragment(
        inline_fragment: &InlineFragment,
        schema: &Valid<Schema>,
    ) -> Result<f64, BoxError> {
        BasicCostCalculator::score_selection_set(&inline_fragment.selection_set, schema)
    }

    fn score_operation(operation: &Operation, schema: &Valid<Schema>) -> Result<f64, BoxError> {
        let mut cost = if operation.is_mutation() { 10.0 } else { 0.0 };
        cost += BasicCostCalculator::score_selection_set(&operation.selection_set, schema)?;

        Ok(cost)
    }

    fn score_selection(selection: &Selection, schema: &Valid<Schema>) -> Result<f64, BoxError> {
        match selection {
            Selection::Field(f) => BasicCostCalculator::score_field(f, schema),
            Selection::FragmentSpread(s) => BasicCostCalculator::score_fragment_spread(s),
            Selection::InlineFragment(i) => BasicCostCalculator::score_inline_fragment(i, schema),
        }
    }

    fn score_selection_set(
        selection_set: &SelectionSet,
        schema: &Valid<Schema>,
    ) -> Result<f64, BoxError> {
        let mut cost = 0.0;
        for selection in selection_set.selections.iter() {
            cost += BasicCostCalculator::score_selection(selection, schema)?;
        }
        Ok(cost)
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
    fn estimated(query: &ExecutableDocument, schema: &Valid<Schema>) -> Result<f64, BoxError> {
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
        let schema = Valid::assume_valid(Schema::parse(schema_str, "").unwrap());
        let query = ExecutableDocument::parse(&schema, query_str, "").unwrap();
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
    fn scalar_list_cost() {
        let schema = "
            type Query {
                numbers: [Int]
            }
        ";
        let query = "
            {
                numbers
            }
        ";

        assert_eq!(cost(schema, query), 0.0)
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

    #[test]
    fn requires_adds_required_field_cost() {
        let schema = r#"
            extend schema
                @link(url: "https://spec.apollo.dev/federation/2.7", import: ["external", "requires"])

            type Query {
                products: [Product!] @external
                productCount: Int! @requires(fields: "products")
            }

            type Product {
                name: String!
            }
        "#;
        let query = "
            {
                productCount
            }
        ";

        assert_eq!(cost(schema, query), 100.0);
    }

    #[test]
    fn nested_requires_adds_required_field_costs() {
        let schema = r#"
            extend schema
                @link(url: "https://spec.apollo.dev/federation/2.7", import: ["external", "requires"])

            type Query {
                foos: [Foo!] @external
                bar: Bar
                thingWithRequires: Int! @requires(fields: "foos bar { bazzes }")
            }

            type Foo {
                name: String!
            }

            type Bar {
                bazzes: [Baz!] @external
            }

            type Baz {
                name: String!
            }
        "#;
        let query = "
            {
                thingWithRequires
            }
        ";

        assert_eq!(cost(schema, query), 201.0);
    }
}
