//! Helper structs & functions for dealing with GraphQL schemas
use std::ops::Deref;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use line_col::LineColLookup;

mod strings;

pub(super) use strings::GraphQLString;

pub(super) struct SchemaInfo<'schema> {
    pub(crate) schema: &'schema Schema,
    len: usize,
    lookup: LineColLookup<'schema>,
    pub(crate) connect_directive_name: &'schema Name,
    pub(crate) source_directive_name: &'schema Name,
}

impl<'schema> SchemaInfo<'schema> {
    pub(crate) fn new(
        schema: &'schema Schema,
        src: &'schema str,
        connect_directive_name: &'schema Name,
        source_directive_name: &'schema Name,
    ) -> Self {
        Self {
            schema,
            len: src.len(),
            lookup: LineColLookup::new(src),
            connect_directive_name,
            source_directive_name,
        }
    }

    /// Get the 1-based line and column values for an offset into this schema.
    ///
    /// # Returns
    /// The line and column, or `None` if the offset is not within the schema.
    pub(crate) fn line_col(&self, offset: usize) -> Option<(usize, usize)> {
        if offset > self.len {
            None
        } else {
            Some(self.lookup.get(offset))
        }
    }
}

impl Deref for SchemaInfo<'_> {
    type Target = Schema;

    fn deref(&self) -> &Self::Target {
        self.schema
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_lookup() {
        let src = r#"
            type Query {
                foo: String
            }
        "#;
        let schema = Schema::parse(src, "testSchema").unwrap();

        let name = "unused".try_into().unwrap();
        let schema_info = SchemaInfo::new(&schema, src, &name, &name);

        assert_eq!(schema_info.line_col(0), Some((1, 1)));
        assert_eq!(schema_info.line_col(4), Some((2, 4)));
        assert_eq!(schema_info.line_col(200), None);
    }
}
