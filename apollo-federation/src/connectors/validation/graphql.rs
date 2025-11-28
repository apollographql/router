//! Helper structs & functions for dealing with GraphQL schemas
use std::ops::Deref;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::collections::IndexMap;
use line_col::LineColLookup;
use shape::Shape;

mod strings;

pub(super) use strings::subslice_location;

use crate::connectors::spec::ConnectLink;

pub(crate) struct SchemaInfo<'schema> {
    pub(crate) schema: &'schema Schema,
    len: usize,
    lookup: LineColLookup<'schema>,
    pub(crate) connect_link: ConnectLink,
    /// A lookup map for the Shapes computed from GraphQL types.
    pub(crate) shape_lookup: IndexMap<String, Shape>,
}

impl<'schema> SchemaInfo<'schema> {
    pub(crate) fn new(
        schema: &'schema Schema,
        src: &'schema str,
        connect_link: ConnectLink,
    ) -> Self {
        Self {
            schema,
            len: src.len(),
            lookup: LineColLookup::new(src),
            connect_link,
            shape_lookup: shape::graphql::shapes_for_schema(schema),
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

    #[inline]
    pub(crate) fn source_directive_name(&self) -> &Name {
        &self.connect_link.source_directive_name
    }

    #[inline]
    pub(crate) fn connect_directive_name(&self) -> &Name {
        &self.connect_link.connect_directive_name
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
            extend schema @link(url: "https://specs.apollo.dev/connect/v0.1")
            type Query {
                foo: String
            }
        "#;
        let schema = Schema::parse(src, "testSchema").unwrap();

        let schema_info =
            SchemaInfo::new(&schema, src, ConnectLink::new(&schema).unwrap().unwrap());

        assert_eq!(schema_info.line_col(0), Some((1, 1)));
        assert_eq!(schema_info.line_col(4), Some((2, 4)));
        assert_eq!(schema_info.line_col(200), None);
    }
}
