//! Query Transformer implementation adding labels to @defer directives to identify deferred responses
//!

use apollo_compiler::ast;
use apollo_compiler::name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use tower::BoxError;

use crate::spec::query::subselections::DEFER_DIRECTIVE_NAME;
use crate::spec::query::transform;
use crate::spec::query::transform::document;
use crate::spec::query::transform::Visitor;

const LABEL_NAME: ast::Name = name!("label");

/// go through the query and adds labels to defer fragments that do not have any
///
/// This is used to uniquely identify deferred responses
pub(crate) fn add_defer_labels(
    schema: &Schema,
    doc: &ast::Document,
) -> Result<ast::Document, BoxError> {
    let mut visitor = Labeler {
        next_label: 0,
        schema,
    };
    document(&mut visitor, doc)
}

pub(crate) struct Labeler<'a> {
    schema: &'a Schema,
    next_label: u32,
}

impl Labeler<'_> {
    fn generate_label(&mut self) -> String {
        let label = self.next_label.to_string();
        self.next_label += 1;
        label
    }
}

impl Visitor for Labeler<'_> {
    fn fragment_spread(
        &mut self,
        def: &ast::FragmentSpread,
    ) -> Result<Option<ast::FragmentSpread>, BoxError> {
        let mut new = transform::fragment_spread(self, def)?.unwrap();
        directives(self, &mut new.directives)?;
        Ok(Some(new))
    }

    fn inline_fragment(
        &mut self,
        parent_type: &str,
        def: &ast::InlineFragment,
    ) -> Result<Option<ast::InlineFragment>, BoxError> {
        let mut new = transform::inline_fragment(self, parent_type, def)?.unwrap();
        directives(self, &mut new.directives)?;
        Ok(Some(new))
    }

    fn schema(&self) -> &apollo_compiler::Schema {
        self.schema
    }
}

fn directives(
    visitor: &mut Labeler,
    directives: &mut [Node<ast::Directive>],
) -> Result<(), BoxError> {
    for directive in directives {
        if directive.name != DEFER_DIRECTIVE_NAME {
            continue;
        }
        let directive = directive.make_mut();
        let mut has_label = false;
        for arg in &mut directive.arguments {
            if arg.name == LABEL_NAME {
                has_label = true;
                if let ast::Value::String(label) = arg.make_mut().value.make_mut() {
                    // Add a prefix to existing labels
                    *label = format!("_{label}").into();
                } else {
                    return Err("@defer with a non-string label".into());
                }
            }
        }
        // Add a generated label if there wasn’t one already
        if !has_label {
            directive.arguments.push(
                ast::Argument {
                    name: LABEL_NAME,
                    value: visitor.generate_label().into(),
                }
                .into(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    use super::add_defer_labels;

    #[test]
    fn large_float_written_as_int() {
        let schema = "type Query { field(id: Float): String! }";
        let query = r#"{ field(id: 1234567890123) }"#;
        let schema = apollo_compiler::Schema::parse(schema, "schema.graphql").unwrap();
        let doc = apollo_compiler::ast::Document::parse(query, "query.graphql").unwrap();
        let result = add_defer_labels(&schema, &doc).unwrap().to_string();
        insta::assert_snapshot!(result);
    }
}
