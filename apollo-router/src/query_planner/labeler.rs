//! Query Transformer implementation adding labels to @defer directives to identify deferred responses
//!

use std::collections::HashSet;

use apollo_compiler::hir::Value;
use apollo_compiler::ApolloCompiler;
use apollo_compiler::FileId;
use apollo_encoder::Argument;
use rand::distributions::Alphanumeric;
use rand::rngs::ThreadRng;
use rand::thread_rng;
use rand::Rng;

use crate::spec::query::transform::directive;
use crate::spec::query::transform::document;
use crate::spec::query::transform::selection_set;
use crate::spec::query::transform::Visitor;

/// go through the query and adds labels to defer fragments that do not have any
///
/// This is used to uniquely identify deferred responses
pub(crate) fn add_defer_labels(
    file_id: FileId,
    compiler: &ApolloCompiler,
) -> Result<(String, HashSet<String>), tower::BoxError> {
    let mut reserved_labels = HashSet::new();
    loop {
        let mut visitor = Labeler::new(reserved_labels, compiler);
        match document(&mut visitor, file_id) {
            Ok(modified_query) => {
                let (_, added_labels) = visitor.unpack();
                return Ok((modified_query.to_string(), added_labels));
            }
            Err(e) => {
                // this can happen if one of the added labels is already used somewhere in the query
                if e.to_string() == "label collision" {
                    let (new_reserved_labels, _) = visitor.unpack();
                    reserved_labels = new_reserved_labels;

                    continue;
                } else {
                    return Err(e);
                }
            }
        }
    }
}
pub(crate) struct Labeler<'a> {
    reserved_labels: HashSet<String>,
    added_labels: HashSet<String>,
    compiler: &'a ApolloCompiler,
    rng: ThreadRng,
}

impl<'a> Labeler<'a> {
    fn new(reserved_labels: HashSet<String>, compiler: &'a ApolloCompiler) -> Self {
        Self {
            reserved_labels,
            added_labels: HashSet::new(),
            compiler,
            rng: thread_rng(),
        }
    }

    fn unpack(self) -> (HashSet<String>, HashSet<String>) {
        (self.reserved_labels, self.added_labels)
    }

    fn generate_label(&mut self) -> String {
        loop {
            let new_label: String = (&mut self.rng)
                .sample_iter(Alphanumeric)
                .take(12)
                .map(char::from)
                .collect();

            if self.reserved_labels.contains(&new_label) {
                continue;
            }

            self.added_labels.insert(new_label.clone());
            return new_label;
        }
    }
}

impl<'a> Visitor for Labeler<'a> {
    fn compiler(&self) -> &apollo_compiler::ApolloCompiler {
        self.compiler
    }

    fn fragment_spread(
        &mut self,
        hir: &apollo_compiler::hir::FragmentSpread,
    ) -> Result<Option<apollo_encoder::FragmentSpread>, tower::BoxError> {
        let name = hir.name();
        let mut encoder_node = apollo_encoder::FragmentSpread::new(name.into());
        for hir in hir.directives() {
            let is_defer = hir.name() == "defer";
            let has_label = hir.argument_by_name("label").and_then(|v| match v {
                Value::String(s) => Some(s),
                _ => None,
            });
            if let Some(mut d) = directive(hir)? {
                if is_defer {
                    match has_label {
                        Some(label) => {
                            if self.added_labels.contains(label) {
                                return Err("label collision".into());
                            }
                            self.reserved_labels.insert(label.clone());
                        }
                        None => {
                            let label = self.generate_label();

                            d.arg(Argument::new("label".to_string(), label.into()));
                        }
                    }
                }
                encoder_node.directive(d)
            }
        }
        Ok(Some(encoder_node))
    }

    fn inline_fragment(
        &mut self,
        parent_type: &str,
        hir: &apollo_compiler::hir::InlineFragment,
    ) -> Result<Option<apollo_encoder::InlineFragment>, tower::BoxError> {
        let parent_type = hir.type_condition().unwrap_or(parent_type);

        let Some(selection_set) = selection_set(self, hir.selection_set(), parent_type)?
    else { return Ok(None) };

        let mut encoder_node = apollo_encoder::InlineFragment::new(selection_set);

        encoder_node.type_condition(
            hir.type_condition()
                .map(|name| apollo_encoder::TypeCondition::new(name.into())),
        );

        for hir in hir.directives() {
            let is_defer = hir.name() == "defer";
            let has_label = hir.argument_by_name("label").and_then(|v| match v {
                Value::String(s) => Some(s),
                _ => None,
            });
            if let Some(mut d) = directive(hir)? {
                if is_defer {
                    match has_label {
                        Some(label) => {
                            if self.added_labels.contains(label) {
                                return Err("label collision".into());
                            }
                            self.reserved_labels.insert(label.clone());
                        }
                        None => {
                            let label = self.generate_label();

                            d.arg(Argument::new("label".to_string(), label.into()));
                        }
                    }
                }
                encoder_node.directive(d)
            }
        }
        Ok(Some(encoder_node))
    }
}
