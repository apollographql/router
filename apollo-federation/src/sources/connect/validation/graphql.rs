use std::ops::Deref;

///! Helper structs & functions for dealing with GraphQL schemas
use apollo_compiler::Name;
use apollo_compiler::Schema;
use line_col::LineColLookup;

mod strings;

pub(super) use strings::GraphQLString;

pub(super) struct SchemaInfo<'schema> {
    pub(crate) schema: &'schema Schema,
    pub(crate) lookup: LineColLookup<'schema>,
    pub(crate) connect_directive_name: &'schema Name,
    pub(crate) source_directive_name: &'schema Name,
}

impl Deref for SchemaInfo<'_> {
    type Target = Schema;

    fn deref(&self) -> &Self::Target {
        self.schema
    }
}
