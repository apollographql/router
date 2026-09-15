//! Helper structs & functions for dealing with GraphQL schemas
use std::ops::Deref;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::collections::IndexMap;
use shape::Shape;

mod strings;

pub(super) use strings::subslice_location;

use crate::connectors::spec::ConnectLink;
use crate::schema::FederationSchema;

pub(crate) struct SchemaInfo<'schema> {
    federation_schema: &'schema FederationSchema,
    pub(crate) connect_link: ConnectLink,
    /// A lookup map for the Shapes computed from GraphQL types.
    pub(crate) shape_lookup: IndexMap<String, Shape>,
}

impl<'schema> SchemaInfo<'schema> {
    pub(crate) fn new(
        federation_schema: &'schema FederationSchema,
        connect_link: ConnectLink,
    ) -> Self {
        Self {
            federation_schema,
            connect_link,
            shape_lookup: shape::graphql::shapes_for_schema(federation_schema.schema()),
        }
    }

    /// The schema plus the federation metadata computed during link expansion: link metadata,
    /// referencers, and subgraph metadata.
    ///
    /// Prefer this over the raw [`Schema`] — it resolves federation directive names through their
    /// `@link` imports, so renames and aliases are handled.
    #[inline]
    pub(crate) fn federation_schema(&self) -> &'schema FederationSchema {
        self.federation_schema
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
        self.federation_schema.schema()
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::parser::LineColumn;

    use super::*;

    /// Locations now come from the schema's own [`SourceMap`], so they agree with every other
    /// message this module produces. This pins the offset-to-line/column mapping that
    /// `subslice_location` depends on.
    #[test]
    fn line_col_lookup() {
        let src = r#"
            extend schema @link(url: "https://specs.apollo.dev/connect/v0.1")
            type Query {
                foo: String
            }
        "#;
        let schema = Schema::parse(src, "testSchema").unwrap();
        let file = schema
            .sources
            .values()
            .find(|file| file.path() == std::path::Path::new("testSchema"))
            .unwrap();

        assert_eq!(
            file.get_line_column(0),
            Some(LineColumn { line: 1, column: 1 })
        );
        assert_eq!(
            file.get_line_column(4),
            Some(LineColumn { line: 2, column: 4 })
        );
        assert_eq!(file.get_line_column(200), None);
    }
}
