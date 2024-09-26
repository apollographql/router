//! Helpers for dealing with GraphQL literal strings and getting locations within them.
//!
//! Specifically, working around these issues with `apollo-compiler` which make determining the
//! line/column of locations _within_ a string impossible:
//!
//! 1. Quotes are included in locations (i.e., `line_column_range`) but not the values from `Node<Value>`.
//! 2. String values in `Node<Value>` are stripped of insignificant whitespace

use std::ops::Range;

use apollo_compiler::ast::Value;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::parser::SourceMap;
use apollo_compiler::Node;

use crate::sources::connect::validation::graphql::SchemaInfo;

#[derive(Clone, Copy)]
pub(crate) struct GraphQLString<'schema> {
    /// The GraphQL String literal without quotes, but with all whitespace intact
    raw_string: &'schema str,
    /// Where `raw_string` _starts_ in the source text
    offset: usize,
}

impl<'schema> GraphQLString<'schema> {
    /// Get the raw string value of this GraphQL string literal
    ///
    /// Returns `None` if the value was not a string literal or if location data is messed up
    pub fn new(value: &'schema Node<Value>, sources: &'schema SourceMap) -> Result<Self, ()> {
        let value_without_quotes = value.as_str().ok_or(())?;
        let source_span = value.location().ok_or(())?;
        let file = sources.get(&source_span.file_id()).ok_or(())?;
        let source_text = file.source_text();
        let start_of_quotes = source_span.offset();
        let end_of_quotes = source_span.end_offset();
        let value_with_quotes = source_text.get(start_of_quotes..end_of_quotes).ok_or(())?;

        // On each line, the whitespace gets messed up for multi-line strings
        // So we find the first and last lines of the parsed value (no whitespace) within
        // the raw value (with whitespace) to get our raw string.
        let first_line_of_value = value_without_quotes.lines().next().ok_or(())?;
        let start_of_value = value_with_quotes.find(first_line_of_value).ok_or(())?;
        let last_line_of_value = value_without_quotes.lines().last().ok_or(())?;
        let end_of_value =
            value_with_quotes.rfind(last_line_of_value).ok_or(())? + last_line_of_value.len();
        let raw_string = value_with_quotes
            .get(start_of_value..end_of_value)
            .ok_or(())?;

        Ok(Self {
            raw_string,
            offset: start_of_quotes + start_of_value,
        })
    }

    pub fn as_str(&self) -> &str {
        self.raw_string
    }

    pub fn line_col_for_subslice(
        &self,
        substring_location: Range<usize>,
        schema_info: &SchemaInfo,
    ) -> Option<Range<LineColumn>> {
        let start_offset = self.offset + substring_location.start;
        let end_offset = self.offset + substring_location.end;

        let (line, column) = schema_info.line_col(start_offset)?;
        let start = LineColumn { line, column };
        let (line, column) = schema_info.line_col(end_offset)?;
        let end = LineColumn { line, column };

        Some(start..end)
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::ast::Value;
    use apollo_compiler::parser::LineColumn;
    use apollo_compiler::schema::ExtendedType;
    use apollo_compiler::Node;
    use apollo_compiler::Schema;
    use pretty_assertions::assert_eq;

    use crate::sources::connect::validation::graphql::GraphQLString;
    use crate::sources::connect::validation::graphql::SchemaInfo;

    const SCHEMA: &str = r#"
        type Query {
          field: String @connect(
            http: {
                GET: "https://example.com"
            },
            selection: """
            something
            somethingElse {
              nested
            }
            """
          )
        }
        "#;

    fn connect_argument<'schema>(schema: &'schema Schema, name: &str) -> &'schema Node<Value> {
        let ExtendedType::Object(query) = schema.types.get("Query").unwrap() else {
            panic!("Query type not found");
        };
        let field = query.fields.get("field").unwrap();
        let directive = field.directives.get("connect").unwrap();
        directive.argument_by_name(name, schema).unwrap()
    }

    #[test]
    fn single_quoted_string() {
        let schema = Schema::parse(SCHEMA, "test.graphql").unwrap();
        let http = connect_argument(&schema, "http").as_object().unwrap();
        let value = &http[0].1;

        let string = GraphQLString::new(value, &schema.sources).unwrap();
        assert_eq!(string.as_str(), "https://example.com");
        let name = "unused".try_into().unwrap();
        let schema_info = SchemaInfo::new(&schema, SCHEMA, &name, &name);
        assert_eq!(
            string.line_col_for_subslice(2..5, &schema_info),
            Some(
                LineColumn {
                    line: 5,
                    column: 25
                }..LineColumn {
                    line: 5,
                    column: 28
                }
            )
        );
    }

    #[test]
    fn multi_line_string() {
        let schema = Schema::parse(SCHEMA, "test.graphql").unwrap();
        let value = connect_argument(&schema, "selection");

        let string = GraphQLString::new(value, &schema.sources).unwrap();
        assert_eq!(
            string.as_str(),
            r#"something
            somethingElse {
              nested
            }"#
        );
        let name = "unused".try_into().unwrap();
        let schema_info = SchemaInfo::new(&schema, SCHEMA, &name, &name);
        assert_eq!(
            string.line_col_for_subslice(8..16, &schema_info),
            Some(
                LineColumn {
                    line: 8,
                    column: 21
                }..LineColumn { line: 9, column: 7 }
            )
        );
    }
}
