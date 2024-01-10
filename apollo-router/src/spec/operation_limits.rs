use std::collections::HashMap;
use std::collections::HashSet;

use apollo_compiler::executable;
use apollo_compiler::ExecutableDocument;
use serde::Deserialize;
use serde::Serialize;

use crate::Configuration;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) struct OperationLimits<T> {
    pub(crate) depth: T,
    pub(crate) height: T,
    pub(crate) root_fields: T,
    pub(crate) aliases: T,
}

/// If it swims like a burrito and quacks like a burrito…
impl<A> OperationLimits<A> {
    fn map<B>(self, mut f: impl FnMut(A) -> B) -> OperationLimits<B> {
        OperationLimits {
            depth: f(self.depth),
            height: f(self.height),
            root_fields: f(self.root_fields),
            aliases: f(self.aliases),
        }
    }

    fn combine<B, C>(
        self,
        other: OperationLimits<B>,
        mut f: impl FnMut(&'static str, A, B) -> C,
    ) -> OperationLimits<C> {
        OperationLimits {
            depth: f("depth", self.depth, other.depth),
            height: f("height", self.height, other.height),
            root_fields: f("root_fields", self.root_fields, other.root_fields),
            aliases: f("aliases", self.aliases, other.aliases),
        }
    }
}

impl OperationLimits<bool> {
    fn any(&self) -> bool {
        // make the compile warn if we forget one
        let Self {
            depth,
            height,
            root_fields,
            aliases,
        } = *self;
        depth || height || root_fields || aliases
    }
}

/// Returns which limits are exceeded by the given query, if any
pub(crate) fn check(
    configuration: &Configuration,
    query: &str,
    document: &ExecutableDocument,
    operation_name: Option<&str>,
) -> Result<(), OperationLimits<bool>> {
    let config_limits = &configuration.limits;
    let max = OperationLimits {
        depth: config_limits.max_depth,
        height: config_limits.max_height,
        root_fields: config_limits.max_root_fields,
        aliases: config_limits.max_aliases,
    };
    if !max.map(|limit| limit.is_some()).any() {
        // No configured limit
        return Ok(());
    }

    let Ok(operation) = document.get_operation(operation_name) else {
        // Undefined or ambiguous operation name.
        // The request is invalid and will be rejected by some other part of the router,
        // if it wasn’t already before we got to this code path.
        return Ok(());
    };

    let mut fragment_cache = HashMap::new();
    let measured = count(document, &mut fragment_cache, &operation.selection_set);
    let exceeded = max.combine(measured, |_, config, measured| {
        if let Some(limit) = config {
            measured > limit
        } else {
            false
        }
    });
    if exceeded.any() {
        let mut messages = Vec::new();
        max.combine(measured, |ident, max, measured| {
            if let Some(max) = max {
                if measured > max {
                    messages.push(format!("{ident}: {measured}, max_{ident}: {max}"))
                }
            }
        });
        let message = messages.join(", ");
        tracing::warn!(
            "request exceeded complexity limits: {message}, \
            query: {query:?}, operation name: {operation_name:?}"
        );
        if !config_limits.warn_only {
            return Err(exceeded);
        }
    }
    Ok(())
}

enum Computation<T> {
    InProgress,
    Done(T),
}

/// Recursively measure the given selection set against each limit
fn count<'a>(
    document: &'a executable::ExecutableDocument,
    fragment_cache: &mut HashMap<&'a executable::Name, Computation<OperationLimits<u32>>>,
    selection_set: &'a executable::SelectionSet,
) -> OperationLimits<u32> {
    let mut counts = OperationLimits {
        depth: 0,
        height: 0,
        root_fields: 0,
        aliases: 0,
    };
    let mut fields_seen = HashSet::new();
    for selection in &selection_set.selections {
        match selection {
            executable::Selection::Field(field) => {
                let nested = count(document, fragment_cache, &field.selection_set);
                counts.depth = counts.depth.max(1 + nested.depth);
                counts.height += nested.height;
                counts.aliases += nested.aliases;
                // Multiple aliases for the same field could use different arguments
                // Until we do full merging for limit checking purpose,
                // approximate measured height with an upper bound rather than a lower bound.
                let used_name = if let Some(alias) = &field.alias {
                    counts.aliases += 1;
                    alias
                } else {
                    &field.name
                };
                let not_seen_before = fields_seen.insert(used_name);
                if not_seen_before {
                    counts.height += 1;
                    counts.root_fields += 1;
                }
            }
            executable::Selection::InlineFragment(fragment) => {
                let nested = count(document, fragment_cache, &fragment.selection_set);
                counts.depth = counts.depth.max(nested.depth);
                counts.height += nested.height;
                counts.aliases += nested.aliases;
            }
            executable::Selection::FragmentSpread(fragment) => {
                let name = &fragment.fragment_name;
                let nested;
                match fragment_cache.get(name) {
                    None => {
                        if let Some(definition) = document.fragments.get(name) {
                            fragment_cache.insert(name, Computation::InProgress);
                            nested = count(document, fragment_cache, &definition.selection_set);
                            fragment_cache.insert(name, Computation::Done(nested));
                        } else {
                            // Undefined fragment. The operation invalid
                            // and will be rejected by some other part of the router,
                            // if it wasn’t already before we got to this code path.
                            continue;
                        }
                    }
                    Some(Computation::InProgress) => {
                        // This fragment references itself (maybe indirectly).
                        // https://spec.graphql.org/October2021/#sec-Fragment-spreads-must-not-form-cycles
                        // The operation invalid
                        // and will be rejected by some other part of the router,
                        // if it wasn’t already before we got to this code path.
                        continue;
                    }
                    Some(Computation::Done(cached)) => nested = *cached,
                }
                counts.depth = counts.depth.max(nested.depth);
                counts.height += nested.height;
                counts.aliases += nested.aliases;
            }
        }
    }
    counts
}
