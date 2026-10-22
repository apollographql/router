//! Custom `->` method definitions registered via `@source(methods:)` (and, in a
//! later step, the `@method` directive).
//!
//! A method is a **name for an inlinable JSONSelection** over its input. Invoking
//! `input->Name(args)` evaluates the method's body with `@` bound to the receiver
//! (`input`) and the positional parameters bound as locals, while `$` *lags* —
//! it keeps the caller's ambient root rather than being clamped to the receiver.
//! That is what makes the body mean exactly what the equivalent inline selection
//! would mean at the call site (the Splice Correspondence Principle), and it is
//! why a method body refers to its own input as `@`, not `$`. See
//! `apply_custom_method` for the frame construction. Methods are global,
//! flat-namespaced, and closure-free; a set of methods is admitted iff every method
//! could be inlined (its full expansion is finite — i.e. the call graph is a
//! DAG).
//!
//! This module owns the compiled representation ([`CompiledMethod`]). Building the
//! global registry, definition-site validation, and the compose-time cycle
//! check land alongside it as the compose-side work proceeds.

use std::collections::HashMap;
use std::collections::HashSet;

use indexmap::IndexMap;

use super::JSONSelection;
use super::JSONSelectionParseError;
use super::methods::is_reserved_method_name;
use crate::connectors::ConnectSpec;

/// A single compiled custom `->` method definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompiledMethod {
    /// The method's name — the `@source(methods:)` key, and the method name at the
    /// `->Name` call site.
    pub(crate) name: String,

    /// Positional parameter names, without their leading `$`, in declaration
    /// order. Empty for a nullary method.
    pub(crate) params: Vec<String>,

    /// The parsed body: a complete JSONSelection, evaluated over the method
    /// input with `params` bound as locals.
    pub(crate) body: JSONSelection,

    /// True when this method was derived from a GraphQL type (via `@method`)
    /// rather than written by hand in a `methods:` block.
    ///
    /// Such a method describes the selection for **one** object of that type, so
    /// applying it to a list is a mistake, and callers reject it with a message
    /// pointing at `->map`. A hand-written method gets no such check: it may
    /// legitimately want the array whole (`sum`, and anything reducing).
    ///
    /// Note this is recorded at derivation time, never inferred from the body's
    /// shape. Inferring would resurrect the accident this exists to catch —
    /// a field-list body silently distributes over an array on its own, so
    /// "looks like a selection" is not evidence of intent.
    pub(crate) derived_from_type: bool,
}

impl CompiledMethod {
    /// Parse and compile a single method from its `@source(methods:)` value — the
    /// string body, including any leading `($a, $b) =>` parameter header.
    pub(crate) fn parse(
        name: impl Into<String>,
        body: &str,
        spec: ConnectSpec,
    ) -> Result<Self, JSONSelectionParseError> {
        let (params, body) = JSONSelection::parse_def_with_spec(body, spec)?;
        Ok(Self {
            name: name.into(),
            params,
            body,
            derived_from_type: false,
        })
    }

    /// Mark this method as derived from a GraphQL type — see
    /// [`Self::derived_from_type`].
    pub(crate) fn derived_from_type(mut self) -> Self {
        self.derived_from_type = true;
        self
    }
}

/// An error detected while building the global method registry. These are
/// compose-time errors: the validation layer maps each to a user-facing
/// diagnostic. Phrasing intentionally speaks in *inlinability* terms — the
/// invariant the user cares about — rather than graph jargon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MethodError {
    /// Two methods share the same name (across all `methods:` blocks).
    DuplicateName { name: String },

    /// A method's name collides with a *reserved* built-in — one whose meaning is
    /// fixed by the language rather than by a function body (see
    /// [`is_reserved_method_name`]). Ordinary built-ins may be shadowed; these
    /// may not.
    ShadowsReserved { name: String },

    // Note: shadowing an *ordinary* built-in is not a `MethodError` at all. It is
    // legal, the method wins, and the advisory warning is raised by the validation
    // layer — `MethodError` is reserved for conditions that make a registry
    // unrepresentable.
    /// A set of methods refer to one another in a cycle, so they cannot be
    /// inlined. `cycle` lists the names in call order, closing the loop — e.g.
    /// `["a", "b", "a"]` for `a -> b -> a`, or `["a", "a"]` for a self-call.
    NotInlinable { cycle: Vec<String> },
}

/// The global registry of custom `->` method definitions, merged from every
/// `@source(methods:)`, every `@connect(methods:)`, and every `@method` in the
/// schema.
///
/// The registry is only constructible via `MethodRegistry::build`, which rejects
/// duplicates and any cyclic (non-inlinable) set — so a **cyclic registry is
/// unrepresentable** rather than merely validated somewhere. That makes the
/// cycle check unskippable by construction, no matter which schema-ingestion
/// path builds it.
///
/// # Shadowing an ordinary built-in is allowed, and the method wins
///
/// A method may reuse the name of a built-in `->method`, and callers resolve to the
/// method rather than the built-in (see the dispatch sites in `apply_to.rs`).
///
/// This is deliberate, and it is about forward compatibility rather than
/// expressiveness. Authors reach for a custom method precisely when a desirable
/// built-in is *missing*, and they naturally give it the obvious name — which is
/// the same name we would pick when we later ship that built-in ourselves. If
/// built-ins won, shipping one would silently swap the meaning of an existing
/// method; if shadowing were an error, shipping one would fail composition for
/// schemas that already compose today. Either way a new built-in would be a
/// breaking change. Letting the method win keeps new built-ins purely additive:
/// existing schemas keep the semantics they wrote and tested, and only schemas
/// that *don't* define the name pick up the new built-in.
///
/// The cost is that such a method permanently hides the built-in within that
/// subgraph, and the author would otherwise never learn that a built-in of that
/// name had appeared. Validation therefore emits a `Severity::Warning` for the
/// shadowing — non-fatal, but visible — so "nothing breaks" does not become
/// "nobody ever finds out."
///
/// # Reserved names are the exception
///
/// Built-ins whose meaning is fixed by the language rather than by a function
/// body cannot be shadowed; `MethodError::ShadowsReserved` rejects them. See
/// `methods::is_reserved_method_name` for why `->as` qualifies. The
/// forward-compatibility argument above does not apply to these: they already
/// mean something today, so reserving them cannot retroactively break a schema.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MethodRegistry {
    methods: IndexMap<String, CompiledMethod>,
}

impl MethodRegistry {
    /// Build the registry from all declared methods. Collects *all* errors rather
    /// than stopping at the first, so composition can report duplicates,
    /// shadows, and cycles together.
    pub(crate) fn build(
        methods: impl IntoIterator<Item = CompiledMethod>,
    ) -> Result<Self, Vec<MethodError>> {
        let mut map: IndexMap<String, CompiledMethod> = IndexMap::new();
        let mut errors = Vec::new();

        for method in methods {
            if is_reserved_method_name(&method.name) {
                errors.push(MethodError::ShadowsReserved {
                    name: method.name.clone(),
                });
                continue;
            }
            if map.contains_key(&method.name) {
                errors.push(MethodError::DuplicateName {
                    name: method.name.clone(),
                });
                continue;
            }
            map.insert(method.name.clone(), method);
        }

        // The cycle check runs over the de-duplicated registry. It lives inside
        // the builder so the DAG invariant holds for every constructed registry.
        if let Some(cycle) = find_cycle(&map) {
            errors.push(MethodError::NotInlinable { cycle });
        }

        if errors.is_empty() {
            Ok(Self { methods: map })
        } else {
            Err(errors)
        }
    }

    // Consumed by runtime dispatch (step 2) and the shape pass (step 3).
    #[allow(dead_code)]
    pub(crate) fn get(&self, name: &str) -> Option<&CompiledMethod> {
        self.methods.get(name)
    }

    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.methods.is_empty()
    }

    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.methods.len()
    }
}

/// Find a cycle in the method call graph via three-color DFS, returning the first
/// cycle as an ordered, loop-closing list of names (`a -> b -> a` becomes
/// `["a", "b", "a"]`). Returns `None` when the methods form a DAG.
///
/// Edges are built from each body's *exhaustive* method-call walk
/// ([`JSONSelection::method_calls`]) filtered to names that are themselves
/// methods; builtins and unknown names are not edges. Detecting a cycle is exactly
/// detecting non-inlinability — a method denotes a finite expansion iff its call
/// graph is acyclic — so this is the invariant checked exactly, not a proxy.
fn find_cycle(methods: &IndexMap<String, CompiledMethod>) -> Option<Vec<String>> {
    // Adjacency over method names only (deduplicated, preserving first-seen order).
    let mut edges: HashMap<&str, Vec<&str>> = HashMap::with_capacity(methods.len());
    for (name, method) in methods {
        let mut seen = HashSet::new();
        let mut callees = Vec::new();
        for called in method.body.method_calls() {
            let callee = called.as_ref().as_str();
            if methods.contains_key(callee) && seen.insert(callee) {
                callees.push(callee);
            }
        }
        edges.insert(name.as_str(), callees);
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Color {
        White,
        Gray,
        Black,
    }
    let mut color: HashMap<&str, Color> =
        methods.keys().map(|k| (k.as_str(), Color::White)).collect();

    for start in methods.keys().map(|k| k.as_str()) {
        if color.get(start).copied() != Some(Color::White) {
            continue;
        }
        // Iterative DFS. `path` is the current gray stack; `idx` tracks the
        // next child to visit for each frame (kept in lockstep with `path`).
        let mut path: Vec<&str> = vec![start];
        let mut idx: Vec<usize> = vec![0];
        color.insert(start, Color::Gray);

        while let (Some(&node), Some(&i)) = (path.last(), idx.last()) {
            let next_child = edges
                .get(node)
                .and_then(|children| children.get(i))
                .copied();
            let Some(child) = next_child else {
                // Exhausted this frame's children: mark black and backtrack.
                color.insert(node, Color::Black);
                path.pop();
                idx.pop();
                continue;
            };
            if let Some(slot) = idx.last_mut() {
                *slot += 1;
            }
            match color.get(child).copied() {
                Some(Color::White) => {
                    color.insert(child, Color::Gray);
                    path.push(child);
                    idx.push(0);
                }
                Some(Color::Gray) => {
                    // Back edge: the cycle runs from `child`'s position in the
                    // gray stack to `node`, then closes back on `child`.
                    let from = path.iter().position(|n| *n == child).unwrap_or(0);
                    let mut cycle: Vec<String> =
                        path.iter().skip(from).map(|s| s.to_string()).collect();
                    cycle.push(child.to_string());
                    return Some(cycle);
                }
                _ => {}
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn method(body: &str) -> CompiledMethod {
        CompiledMethod::parse("test", body, ConnectSpec::V0_5).expect("method should parse")
    }

    #[test]
    fn nullary_def_has_no_params() {
        let d = method("id name email: emailAddress");
        assert!(d.params.is_empty());
        assert_eq!(d.name, "test");
    }

    #[test]
    fn parameter_header_is_stripped_and_collected() {
        let d = method("($first, $last) => { first: $first last: $last }");
        assert_eq!(d.params, vec!["first".to_string(), "last".to_string()]);
    }

    #[test]
    fn header_whitespace_and_comments_are_tolerated() {
        let d = method("(  $a ,\n  $b  ) => $a->add($b)");
        assert_eq!(d.params, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn leading_paren_without_arrow_is_an_error() {
        // A leading `(` commits to header parsing; a missing `=>` is a hard
        // error rather than a silent reparse as a body.
        assert!(CompiledMethod::parse("test", "($a) $a", ConnectSpec::V0_5).is_err());
    }

    #[test]
    fn body_referencing_param_parses() {
        // `$other` resolves as a Local because it is seeded from the header.
        let d = method("($other) => $->add($other)");
        assert_eq!(d.params, vec!["other".to_string()]);
    }

    #[test]
    fn literal_constructing_body_ignores_input() {
        let d = method("{ allowOrigin: '*' maxAge: $(86400) }");
        assert!(d.params.is_empty());
    }

    fn named_method(name: &str, body: &str) -> CompiledMethod {
        CompiledMethod::parse(name, body, ConnectSpec::V0_5).expect("method should parse")
    }

    #[test]
    fn builds_a_valid_dag() {
        // a -> b, b -> (nothing). Acyclic, so it builds.
        let registry =
            MethodRegistry::build([named_method("a", "$->b"), named_method("b", "id name")])
                .expect("DAG should build");
        assert_eq!(registry.len(), 2);
        assert!(registry.get("a").is_some());
    }

    #[test]
    fn rejects_duplicate_names() {
        let err = MethodRegistry::build([named_method("a", "id"), named_method("a", "name")])
            .expect_err("duplicate should error");
        assert!(err.contains(&MethodError::DuplicateName {
            name: "a".to_string()
        }));
    }

    #[test]
    fn accepts_builtin_shadow() {
        // Shadowing a built-in is allowed by construction; the method wins at
        // dispatch (see `MethodRegistry` docs and the dispatch sites in
        // `apply_to.rs`). Rejecting it here would make every future built-in a
        // potential breaking change.
        let registry = MethodRegistry::build([named_method("map", "id")])
            .expect("shadowing ->map should be allowed");
        assert!(registry.get("map").is_some());
    }

    #[test]
    fn rejects_self_recursion() {
        let err =
            MethodRegistry::build([named_method("a", "$->a")]).expect_err("self-call should error");
        assert_eq!(
            err,
            vec![MethodError::NotInlinable {
                cycle: vec!["a".to_string(), "a".to_string()]
            }]
        );
    }

    #[test]
    fn rejects_mutual_recursion() {
        // a -> b -> a
        let err = MethodRegistry::build([named_method("a", "$->b"), named_method("b", "$->a")])
            .expect_err("mutual recursion should error");
        let MethodError::NotInlinable { cycle } = &err[0] else {
            panic!("expected NotInlinable, got {err:?}");
        };
        // The cycle closes on itself and names both methods.
        assert_eq!(cycle.first(), cycle.last());
        assert!(cycle.contains(&"a".to_string()));
        assert!(cycle.contains(&"b".to_string()));
    }

    #[test]
    fn detects_cycle_hidden_in_method_args() {
        // The call to `b` hides inside an argument to a builtin, which the
        // exhaustive walk must still find: a -> (b, via ->eq arg) -> a.
        let err =
            MethodRegistry::build([named_method("a", "$->eq($->b)"), named_method("b", "$->a")])
                .expect_err("cycle through a method argument should error");
        assert!(matches!(err[0], MethodError::NotInlinable { .. }));
    }
}
