//! Helpers for dealing with GraphQL literal strings and locations within them.
//!
//! GraphQL string literals can be either standard single-line strings surrounded by a single
//! set of quotes, or a multi-line block string surrounded by triple quotes.
//!
//! Standard strings may contain escape sequences, while block strings contain verbatim text.
//! Block strings additionally have any common indent and leading whitespace lines removed.
//!
//! See: <https://spec.graphql.org/October2021/#sec-String-Value>

use std::ops::Range;

use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::parser::SourceMap;
use nom::AsChar;

use crate::connectors::validation::graphql::SchemaInfo;

const fn is_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t')
}

fn is_whitespace_line(line: &str) -> bool {
    line.is_empty() || line.chars().all(is_whitespace)
}

#[derive(Clone, Copy)]
enum GraphQLString<'schema> {
    Standard {
        data: Data<'schema>,
    },
    Block {
        data: Data<'schema>,

        /// The common indent
        common_indent: usize,
    },
}

#[derive(Clone, Copy)]
struct Data<'schema> {
    /// The original string from the source file, excluding the surrounding quotes
    raw_string: &'schema str,

    /// Where `raw_string` _starts_ in the source text
    raw_offset: usize,
}

impl<'schema> GraphQLString<'schema> {
    fn new(value: &'schema Node<Value>, sources: &'schema SourceMap) -> Result<Self, ()> {
        // Get the raw string value from the source file. This is just the raw string without any
        // of the escape sequence processing or whitespace/newline modifications mentioned above.
        let source_span = value.location().ok_or(())?;
        let file = sources.get(&source_span.file_id()).ok_or(())?;
        let source_text = file.source_text();
        let start_of_quotes = source_span.offset();
        let end_of_quotes = source_span.end_offset();
        let raw_string_with_quotes = source_text.get(start_of_quotes..end_of_quotes).ok_or(())?;

        // Count the number of double-quote characters
        let num_quotes = raw_string_with_quotes
            .chars()
            .take_while(|c| matches!(c, '"'))
            .count();

        // Get the raw string with the quotes removed
        let raw_string = source_text
            .get(start_of_quotes + num_quotes..end_of_quotes - num_quotes)
            .ok_or(())?;

        Ok(if num_quotes == 3 {
            GraphQLString::Block {
                data: Data {
                    raw_string,
                    raw_offset: start_of_quotes + num_quotes,
                },
                common_indent: raw_string
                    .lines()
                    .skip(1)
                    .filter_map(|line| {
                        let length = line.len();
                        let indent = line.chars().take_while(|&c| is_whitespace(c)).count();
                        (indent < length).then_some(indent)
                    })
                    .min()
                    .unwrap_or(0),
            }
        } else {
            GraphQLString::Standard {
                data: Data {
                    raw_string,
                    raw_offset: start_of_quotes + num_quotes,
                },
            }
        })
    }

    fn line_col_for_subslice(
        &self,
        substring_location: Range<usize>,
        schema_info: &SchemaInfo,
    ) -> Option<Range<LineColumn>> {
        let start_offset = self.true_offset(substring_location.start)?;
        let end_offset = self.true_offset(substring_location.end)?;

        let (line, column) = schema_info.line_col(start_offset)?;
        let start = LineColumn { line, column };
        let (line, column) = schema_info.line_col(end_offset)?;
        let end = LineColumn { line, column };

        Some(start..end)
    }

    /// Given an offset into the compiled string, compute the true offset in the raw source string.
    /// See: https://spec.graphql.org/October2021/#sec-String-Value
    fn true_offset(&self, input_offset: usize) -> Option<usize> {
        match self {
            GraphQLString::Standard { data } => {
                // For standard strings, handle escape sequences
                let mut i = 0usize;
                let mut true_offset = data.raw_offset;
                let mut chars = data.raw_string.chars();
                while i < input_offset {
                    let ch = chars.next()?;
                    true_offset += 1;
                    if ch == '\\' {
                        let next = chars.next()?;
                        true_offset += 1;
                        if next == 'u' {
                            // Determine the length of the codepoint in bytes. For example, \uFDFD
                            // is 3 bytes when encoded in UTF-8 (0xEF,0xB7,0xBD).
                            let codepoint: String = (&mut chars).take(4).collect();
                            let codepoint = u32::from_str_radix(&codepoint, 16).ok()?;
                            i += char::from_u32(codepoint)?.len();
                            true_offset += 4;
                            continue;
                        }
                    }
                    i += ch.len();
                }
                Some(true_offset)
            }
            GraphQLString::Block {
                data,
                common_indent,
            } => {
                // For block strings, handle whitespace changes
                let mut skip_chars = 0usize;
                let mut skip_lines = data
                    .raw_string
                    .lines()
                    .take_while(|&line| is_whitespace_line(line))
                    .count();
                let mut i = 0usize;
                let mut true_offset = data.raw_offset;
                let mut chars = data.raw_string.chars();
                while i < input_offset {
                    let ch = chars.next()?;
                    true_offset += 1;
                    if skip_chars > 0 {
                        if ch == '\n' {
                            skip_chars = *common_indent;
                            i += 1;
                        } else {
                            skip_chars -= 1;
                        }
                        continue;
                    }
                    if skip_lines > 0 {
                        if ch == '\n' {
                            skip_lines -= 1;
                            if skip_lines == 0 {
                                skip_chars = *common_indent;
                            }
                        }
                        continue;
                    }
                    if ch == '\n' {
                        skip_chars = *common_indent;
                    }
                    if ch != '\r' {
                        i += ch.len();
                    }
                }
                Some(true_offset + skip_chars)
            }
        }
    }
}

pub(crate) fn subslice_location(
    value: &Node<Value>,
    substring_location: Range<usize>,
    schema: &SchemaInfo,
) -> Option<Range<LineColumn>> {
    GraphQLString::new(value, &schema.sources)
        .ok()
        .and_then(|string| string.line_col_for_subslice(substring_location, schema))
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Node;
    use apollo_compiler::Schema;
    use apollo_compiler::ast::Value;
    use apollo_compiler::parser::LineColumn;
    use apollo_compiler::schema::ExtendedType;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::connectors::validation::ConnectLink;
    use crate::connectors::validation::graphql::SchemaInfo;

    const SCHEMA: &str = r#"extend schema @link(url: "https://specs.apollo.dev/connect/v0.1")
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
        directive.specified_argument_by_name(name).unwrap()
    }

    #[test]
    fn standard_string() {
        let schema = Schema::parse(SCHEMA, "test.graphql").unwrap();
        let http = connect_argument(&schema, "http").as_object().unwrap();
        let value = &http[0].1;

        let string = GraphQLString::new(value, &schema.sources).unwrap();
        let schema_info =
            SchemaInfo::new(&schema, SCHEMA, ConnectLink::new(&schema).unwrap().unwrap());
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
    fn block_string() {
        let schema = Schema::parse(SCHEMA, "test.graphql").unwrap();
        let value = connect_argument(&schema, "selection");

        let string = GraphQLString::new(value, &schema.sources).unwrap();
        let schema_info =
            SchemaInfo::new(&schema, SCHEMA, ConnectLink::new(&schema).unwrap().unwrap());
        assert_eq!(
            string.line_col_for_subslice(28..34, &schema_info),
            Some(
                LineColumn {
                    line: 10,
                    column: 15
                }..LineColumn {
                    line: 10,
                    column: 21
                }
            )
        );
    }
}
