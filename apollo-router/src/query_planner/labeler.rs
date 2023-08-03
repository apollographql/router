//! Query Transformer implementation adding labels to @defer directives to identify deferred responses
//!

use apollo_compiler::ApolloCompiler;
use apollo_compiler::AstDatabase;
use apollo_compiler::FileId;
use apollo_parser::mir;
use apollo_parser::mir::Arc;

use crate::spec::query::subselections::DEFER_DIRECTIVE_NAME;

const LABEL_NAME: &str = "label";

/// go through the query and adds labels to defer fragments that do not have any
///
/// This is used to uniquely identify deferred responses
pub(crate) fn add_defer_labels(
    file_id: FileId,
    compiler: &ApolloCompiler,
) -> Result<String, &'static str> {
    let mut labeler = Labeler { next_label: 0 };
    let mut mir = compiler.db.ast(file_id).into_mir();
    labeler.document(&mut mir)?;
    Ok(mir.serialize().no_indent().to_string())
}

struct Labeler {
    next_label: u32,
}

impl Labeler {
    fn generate_label(&mut self) -> String {
        let label = self.next_label.to_string();
        self.next_label += 1;
        label
    }

    fn document(&mut self, document: &mut mir::Document) -> Result<(), &'static str> {
        for def in &mut document.definitions {
            match def {
                mir::Definition::OperationDefinition(operation) => {
                    self.selection_set(&mut Arc::make_mut(operation).selection_set)?
                }
                mir::Definition::FragmentDefinition(fragment) => {
                    self.selection_set(&mut Arc::make_mut(fragment).selection_set)?
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn selection_set(
        &mut self,
        selection_set: &mut Vec<mir::Selection>,
    ) -> Result<(), &'static str> {
        for selection in selection_set {
            match selection {
                mir::Selection::Field(field) => {
                    let field = Arc::make_mut(field);
                    self.selection_set(&mut field.selection_set)?;
                }
                mir::Selection::InlineFragment(inline_fragment) => {
                    let inline_fragment = Arc::make_mut(inline_fragment);
                    self.selection_set(&mut inline_fragment.selection_set)?;
                    // The @defer directive only applies to inline fragments and fragment spreads
                    self.directives(&mut inline_fragment.directives)?
                }
                mir::Selection::FragmentSpread(fragment_spread) => {
                    let fragment_spread = Arc::make_mut(fragment_spread);
                    // The @defer directive only applies to inline fragments and fragment spreads
                    self.directives(&mut fragment_spread.directives)?
                }
            }
        }
        Ok(())
    }

    fn directives(
        &mut self,
        directives: &mut Vec<Arc<mir::Directive>>,
    ) -> Result<(), &'static str> {
        for directive in directives {
            if directive.name != DEFER_DIRECTIVE_NAME {
                continue;
            }
            let directive = Arc::make_mut(directive);
            let mut has_label = false;
            for (name, value) in &mut directive.arguments {
                if *name != LABEL_NAME {
                    continue;
                }
                has_label = true;
                // Add a prefix to existing labels
                if let mir::Value::String(label) = value {
                    let new_label = format!("_{label}");
                    *label = new_label.into();
                } else {
                    return Err("@defer with a non-string label");
                }
            }
            // Add a generated label if there wasn’t one already
            if !has_label {
                directive.arguments.push((
                    LABEL_NAME.into(),
                    mir::Value::String(self.generate_label().into()),
                ));
            }
        }
        Ok(())
    }
}
