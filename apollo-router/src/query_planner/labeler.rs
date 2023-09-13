//! Query Transformer implementation adding labels to @defer directives to identify deferred responses
//!

use apollo_compiler::hir;
use apollo_compiler::ApolloCompiler;
use apollo_compiler::FileId;
use tower::BoxError;

use crate::spec::query::subselections::DEFER_DIRECTIVE_NAME;
use crate::spec::query::transform;
use crate::spec::query::transform::document;
use crate::spec::query::transform::selection_set;
use crate::spec::query::transform::Visitor;

const LABEL_NAME: &str = "label";

/// go through the query and adds labels to defer fragments that do not have any
///
/// This is used to uniquely identify deferred responses
pub(crate) fn add_defer_labels(
    file_id: FileId,
    compiler: &ApolloCompiler,
) -> Result<String, BoxError> {
    let mut visitor = Labeler {
        compiler,
        next_label: 0,
    };
    let encoder_document = document(&mut visitor, file_id)?;
    Ok(encoder_document.to_string())
}
pub(crate) struct Labeler<'a> {
    compiler: &'a ApolloCompiler,
    next_label: u32,
}

impl<'a> Labeler<'a> {
    fn generate_label(&mut self) -> String {
        let label = self.next_label.to_string();
        self.next_label += 1;
        label
    }
}

impl<'a> Visitor for Labeler<'a> {
    fn compiler(&self) -> &apollo_compiler::ApolloCompiler {
        self.compiler
    }

    fn fragment_spread(
        &mut self,
        hir: &hir::FragmentSpread,
    ) -> Result<Option<apollo_encoder::FragmentSpread>, BoxError> {
        let name = hir.name();
        let mut encoder_node = apollo_encoder::FragmentSpread::new(name.into());
        for hir in hir.directives() {
            encoder_node.directive(directive(self, hir)?);
        }
        Ok(Some(encoder_node))
    }

    fn inline_fragment(
        &mut self,
        parent_type: &str,
        hir: &hir::InlineFragment,
    ) -> Result<Option<apollo_encoder::InlineFragment>, BoxError> {
        let parent_type = hir.type_condition().unwrap_or(parent_type);

        let Some(selection_set) = selection_set(self, hir.selection_set(), parent_type)? else {
            return Ok(None);
        };

        let mut encoder_node = apollo_encoder::InlineFragment::new(selection_set);

        encoder_node.type_condition(
            hir.type_condition()
                .map(|name| apollo_encoder::TypeCondition::new(name.into())),
        );

        for hir in hir.directives() {
            encoder_node.directive(directive(self, hir)?);
        }
        Ok(Some(encoder_node))
    }
}

pub(crate) fn directive(
    visitor: &mut Labeler<'_>,
    hir: &hir::Directive,
) -> Result<apollo_encoder::Directive, BoxError> {
    let name = hir.name().into();
    let is_defer = name == DEFER_DIRECTIVE_NAME;
    let mut encoder_directive = apollo_encoder::Directive::new(name);

    let mut has_label = false;
    for arg in hir.arguments() {
        // Add a prefix to existing labels
        let value = if is_defer && arg.name() == LABEL_NAME {
            has_label = true;
            if let Some(label) = arg.value().as_str() {
                apollo_encoder::Value::String(format!("_{label}"))
            } else {
                return Err("@defer with a non-string label".into());
            }
        } else {
            transform::value(arg.value())?
        };
        encoder_directive.arg(apollo_encoder::Argument::new(arg.name().into(), value));
    }
    // Add a generated label if there wasn’t one already
    if is_defer && !has_label {
        encoder_directive.arg(apollo_encoder::Argument::new(
            LABEL_NAME.into(),
            apollo_encoder::Value::String(visitor.generate_label()),
        ));
    }

    Ok(encoder_directive)
}

#[cfg(test)]
mod tests {
    use apollo_compiler::ApolloCompiler;

    use super::add_defer_labels;

    #[test]
    fn large_float_written_as_int() {
        let mut compiler = ApolloCompiler::new();
        compiler.add_type_system("type Query { field(id: Float): String! }", "schema.graphql");
        let file_id = compiler.add_executable(r#"{ field(id: 1234567890123) }"#, "query.graphql");
        let result = add_defer_labels(file_id, &compiler).unwrap();
        insta::assert_snapshot!(result);
    }
}
