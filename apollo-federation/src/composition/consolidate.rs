//! Satisfiability-only consolidation of connector synthetic subgraphs.
//!
//! Connector expansion (`expand_connectors`) turns each `@connect` into its own synthetic
//! subgraph so satisfiability can reuse the existing subgraph-based machinery. On large
//! connector supergraphs this explodes into thousands of `@join__graph`s, and
//! `validate_satisfiability` over that expansion is the dominant memory/time cost (it
//! builds a federated query graph with a node per type x subgraph, scaling super-linearly).
//!
//! This transform merges the **root-field (keyless) synthetic subgraphs** — those declaring
//! no resolvable `@join__type(T, key: K)` entry point — into one representative per spec-link
//! scope, purely for the satisfiability check. Keyed subgraphs are left untouched. The
//! expanded supergraph the router executes against is **not** affected — this rewrites only
//! the schema handed to `validate_satisfiability`.
//!
//! It works on the parsed AST (apollo-compiler `Schema`) and lets the compiler serialize the
//! result, so it never hand-formats SDL. The only collapsing it does is a structural
//! *participation merge*: after remapping a graph reference to its representative, directive
//! applications that become byte-identical are folded into one (e.g. two
//! `@join__type(graph: REP)` on a type → one). Repeatable directives with distinct arguments
//! (different keys, different graphs) are preserved.
//!
//! ## Soundness
//!
//! Merging join graphs is reachability-*monotonic*: co-locating fields can only add
//! reachability, never remove it, so a bad merge can only ever *mask* a satisfiability error
//! (false pass), never invent one. The only merges that are safe are therefore those that add
//! no reachability at all.
//!
//! We restrict merging to **root-field (keyless) synthetic subgraphs**: graphs that declare no
//! resolvable `@join__type(T, key: K)` entry point. A keyless graph is reachable only at root
//! operation fields, never via an entity key-jump, so co-locating two keyless graphs' fields
//! bypasses no key edge and adds no reachability — the merge is reachability-*preserving* by
//! construction. Keyed graphs are left untouched (each stays its own representative). This is
//! what keeps the transform sound without having to reason about `@key` subtleties: every
//! hazard that could make a keyed merge unsound — `@key(resolvable: false)`, `@external` key
//! fields, `@override`, or a key whose field types differ across subgraphs — attaches to a
//! *keyed* type, which we never merge. Keyless graphs are still not merged across spec-link
//! scopes (a graph's `@join__directive(name: "link")` membership, e.g. connect v0.2 vs v0.3),
//! since the representative would otherwise link the same feature twice. The grouping key is
//! therefore the link scope alone.
//!
//! One further condition: the participation merge folds two graphs' applications for the same
//! `(type, field)` into the representative, which is only well-formed when those applications
//! are byte-identical. If two merge candidates declare the same `(type, field)` with *differing*
//! `@join__field` metadata (e.g. divergent `type:` overrides on a `@shareable` field), folding
//! would leave two conflicting `@join__field(graph: REP)` on one field and make subgraph
//! extraction define that field twice — a spurious composition error. Such graphs are detected
//! and excluded from merging (each keeps its own representative). Connector synthetic subgraphs
//! are field-disjoint, so this never costs the win in practice; it guards the shared-field case.

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::DirectiveList;
use apollo_compiler::schema::ExtendedType;

/// Rewrite an expanded supergraph SDL, merging the root-field (keyless) `@join__graph`s that
/// share a spec-link scope into one representative each; keyed graphs are left untouched.
/// Returns the consolidated SDL (re-parseable as a supergraph). Fails open: if the input can't
/// be parsed, returns it unchanged so satisfiability still runs on the full expansion.
pub(super) fn consolidate_for_satisfiability(expanded_sdl: &str) -> String {
    let mut schema = match Schema::parse(expanded_sdl, "expanded.graphql") {
        Ok(s) => s,
        Err(_) => return expanded_sdl.to_string(),
    };

    let all_graphs: Vec<Name> = match schema.types.get(&name!("join__Graph")) {
        Some(ExtendedType::Enum(e)) => e.values.keys().cloned().collect(),
        _ => return expanded_sdl.to_string(), // not a connector supergraph; nothing to do
    };

    // Graphs with any `@join__type(…, key:)` entry point are keyed and never merge (see the
    // module doc's Soundness section for why keyless-only merging is what makes this safe).
    let mut keyed: HashSet<Name> = HashSet::new();
    for ty in schema.types.values() {
        for d in type_directives(ty)
            .iter()
            .filter(|d| d.name == name!("join__type"))
        {
            if let (Some(graph), Some(_key)) = (arg_enum(d, "graph"), arg_str(d, "key")) {
                keyed.insert(graph.clone());
            }
        }
    }

    // A graph's spec-link scope is the set of `@join__directive(name: "link", args)` entries
    // it belongs to (e.g. connect v0.2 vs v0.3). Merging across scopes would make the
    // representative link the same feature twice, so the scope is the grouping key.
    let mut link_scope: HashMap<Name, BTreeSet<String>> = HashMap::new();
    for d in schema.schema_definition.directives.iter() {
        if d.name != name!("join__directive") || arg_str(d, "name") != Some("link") {
            continue;
        }
        let scope = arg(d, "args").map(|v| v.to_string()).unwrap_or_default();
        let graphs = arg(d, "graphs").and_then(|v| v.as_list());
        for g in graphs.into_iter().flatten().filter_map(|v| v.as_enum()) {
            link_scope
                .entry(g.clone())
                .or_default()
                .insert(scope.clone());
        }
    }

    // Each graph's `@join__field` application (args minus `graph:`) per (type, field), to
    // detect merge candidates declaring the same field with differing metadata (see the module
    // doc's final paragraph).
    let mut field_apps: HashMap<(Name, Name), Vec<(Name, String)>> = HashMap::new();
    for (type_name, ty) in &schema.types {
        for (field_name, dl) in field_directive_lists(ty) {
            for d in dl.iter().filter(|d| d.name == name!("join__field")) {
                if let Some(graph) = arg_enum(d, "graph") {
                    field_apps
                        .entry((type_name.clone(), field_name.clone()))
                        .or_default()
                        .push((graph.clone(), join_field_app(d)));
                }
            }
        }
    }

    // Merge candidates: the keyless graphs, each tagged with its link scope.
    let mut graph_scope: HashMap<Name, BTreeSet<String>> = HashMap::new();
    for g in &all_graphs {
        if !keyed.contains(g) {
            graph_scope.insert(g.clone(), link_scope.get(g).cloned().unwrap_or_default());
        }
    }

    // Taint candidates that would collide: within one scope group, if two members declare the
    // same (type, field) with different applications, every member declaring that field is
    // excluded from merging and keeps its own representative.
    let mut tainted: HashSet<Name> = HashSet::new();
    for apps in field_apps.values() {
        let mut by_scope: HashMap<&BTreeSet<String>, Vec<(&Name, &String)>> = HashMap::new();
        for (g, app) in apps {
            if let Some(scope) = graph_scope.get(g) {
                by_scope.entry(scope).or_default().push((g, app));
            }
        }
        for members in by_scope.values() {
            if members.iter().any(|(_, app)| *app != members[0].1) {
                tainted.extend(members.iter().map(|(g, _)| (*g).clone()));
            }
        }
    }

    // Group the untainted candidates by scope; representative = smallest member.
    let mut groups: HashMap<&BTreeSet<String>, Vec<&Name>> = HashMap::new();
    for (g, scope) in &graph_scope {
        if !tainted.contains(g) {
            groups.entry(scope).or_default().push(g);
        }
    }
    let mut sym2rep: HashMap<Name, Name> = HashMap::new();
    for members in groups.values() {
        let rep = *members.iter().min().unwrap();
        for m in members {
            if *m != rep {
                sym2rep.insert((*m).clone(), rep.clone());
            }
        }
    }
    if sym2rep.is_empty() {
        return expanded_sdl.to_string(); // nothing merges
    }

    // Remap every graph reference to its representative, folding applications that become
    // identical, and prune the merged-away values from the join__Graph enum.
    let join_graph = name!("join__Graph");
    for (type_name, ty) in schema.types.iter_mut() {
        process_schema_dl(type_directives_mut(ty), &sym2rep);
        if let ExtendedType::Enum(t) = ty {
            let t = t.make_mut();
            if *type_name == join_graph {
                t.values.retain(|k, _| !sym2rep.contains_key(k));
            } else {
                for v in t.values.values_mut() {
                    process_ast_dl(&mut v.make_mut().directives, &sym2rep);
                }
            }
        } else {
            for dl in field_directive_lists_mut(ty) {
                process_ast_dl(dl, &sym2rep);
            }
        }
    }

    // schema-level directives (@join__directive(graphs: […]) re-including @link, etc.)
    process_schema_dl(
        &mut schema.schema_definition.make_mut().directives,
        &sym2rep,
    );

    schema.serialize().to_string()
}

fn type_directives(ty: &ExtendedType) -> &DirectiveList {
    match ty {
        ExtendedType::Scalar(t) => &t.directives,
        ExtendedType::Object(t) => &t.directives,
        ExtendedType::Interface(t) => &t.directives,
        ExtendedType::Union(t) => &t.directives,
        ExtendedType::Enum(t) => &t.directives,
        ExtendedType::InputObject(t) => &t.directives,
    }
}

fn type_directives_mut(ty: &mut ExtendedType) -> &mut DirectiveList {
    match ty {
        ExtendedType::Scalar(t) => &mut t.make_mut().directives,
        ExtendedType::Object(t) => &mut t.make_mut().directives,
        ExtendedType::Interface(t) => &mut t.make_mut().directives,
        ExtendedType::Union(t) => &mut t.make_mut().directives,
        ExtendedType::Enum(t) => &mut t.make_mut().directives,
        ExtendedType::InputObject(t) => &mut t.make_mut().directives,
    }
}

/// The (field name, directives) pairs of a type's fields, uniform across the three
/// field-bearing type kinds (empty for the rest).
fn field_directive_lists(
    ty: &ExtendedType,
) -> Box<dyn Iterator<Item = (&Name, &ast::DirectiveList)> + '_> {
    match ty {
        ExtendedType::Object(t) => Box::new(t.fields.iter().map(|(n, f)| (n, &f.directives))),
        ExtendedType::Interface(t) => Box::new(t.fields.iter().map(|(n, f)| (n, &f.directives))),
        ExtendedType::InputObject(t) => Box::new(t.fields.iter().map(|(n, f)| (n, &f.directives))),
        _ => Box::new(std::iter::empty()),
    }
}

/// Mutable counterpart of [`field_directive_lists`].
fn field_directive_lists_mut(
    ty: &mut ExtendedType,
) -> Box<dyn Iterator<Item = &mut ast::DirectiveList> + '_> {
    match ty {
        ExtendedType::Object(t) => Box::new(
            t.make_mut()
                .fields
                .values_mut()
                .map(|f| &mut f.make_mut().directives),
        ),
        ExtendedType::Interface(t) => Box::new(
            t.make_mut()
                .fields
                .values_mut()
                .map(|f| &mut f.make_mut().directives),
        ),
        ExtendedType::InputObject(t) => Box::new(
            t.make_mut()
                .fields
                .values_mut()
                .map(|f| &mut f.make_mut().directives),
        ),
        _ => Box::new(std::iter::empty()),
    }
}

fn arg<'a>(d: &'a Directive, name: &str) -> Option<&'a Node<Value>> {
    d.arguments
        .iter()
        .find(|a| a.name.as_str() == name)
        .map(|a| &a.value)
}

fn arg_enum<'a>(d: &'a Directive, name: &str) -> Option<&'a Name> {
    arg(d, name).and_then(|v| v.as_enum())
}

fn arg_str<'a>(d: &'a Directive, name: &str) -> Option<&'a str> {
    arg(d, name).and_then(|v| v.as_str())
}

/// Serialize a `@join__field` application's arguments *except* `graph:`, so two applications
/// that differ only by which graph they name compare equal. Used to detect divergent metadata
/// (e.g. different `type:` overrides) on a shared (type, field) that would make a
/// participation-merge produce two conflicting `@join__field(graph: REP)` on one field.
fn join_field_app(d: &Directive) -> String {
    let mut parts: Vec<String> = d
        .arguments
        .iter()
        .filter(|a| a.name.as_str() != "graph")
        .map(|a| format!("{}: {}", a.name, a.value))
        .collect();
    parts.sort();
    parts.join(", ")
}

/// Return a copy of `d` with any `graph:` enum arg and `graphs: [...]` list arg remapped to
/// representatives (the latter deduped, since the remap can repeat a representative).
fn remapped_directive(d: &Directive, sym2rep: &HashMap<Name, Name>) -> Directive {
    let mut nd = d.clone();
    for arg in nd.arguments.iter_mut() {
        match arg.name.as_str() {
            "graph" => {
                let rep = arg.value.as_enum().and_then(|g| sym2rep.get(g)).cloned();
                if let Some(rep) = rep {
                    arg.make_mut().value = Value::Enum(rep).into();
                }
            }
            // `graphs: [...]` (e.g. on `@join__directive`): remap each enum, dropping the
            // duplicates the remap creates so a feature isn't linked twice for one graph.
            "graphs" => {
                if let Some(list) = arg.value.as_list() {
                    let mut seen = HashSet::new();
                    let remapped: Vec<Node<Value>> = list
                        .iter()
                        .map(|v| match v.as_enum() {
                            Some(g) => Value::Enum(sym2rep.get(g).unwrap_or(g).clone()).into(),
                            None => v.clone(),
                        })
                        .filter(|v: &Node<Value>| seen.insert(v.to_string()))
                        .collect();
                    arg.make_mut().value = Value::List(remapped).into();
                }
            }
            _ => {}
        }
    }
    nd
}

/// Fold directives that became byte-identical after remapping (e.g. two
/// `@join__type(graph: REP)` on one type → one).
fn dedup(dirs: Vec<Directive>) -> impl Iterator<Item = Directive> {
    let mut seen = HashSet::new();
    dirs.into_iter().filter(move |d| seen.insert(d.to_string()))
}

/// Remap and fold a schema-level (`Component`) directive list.
fn process_schema_dl(dl: &mut DirectiveList, sym2rep: &HashMap<Name, Name>) {
    let remapped = dl.iter().map(|d| remapped_directive(d, sym2rep)).collect();
    dl.0 = dedup(remapped).map(Component::new).collect();
}

/// Remap and fold an AST (`Node`) directive list.
fn process_ast_dl(dl: &mut ast::DirectiveList, sym2rep: &HashMap<Name, Name>) {
    let remapped = dl.iter().map(|d| remapped_directive(d, sym2rep)).collect();
    dl.0 = dedup(remapped).map(Node::new).collect();
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREAMBLE: &str = r#"
schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION) {
  query: Query
}
directive @join__graph(name: String!, url: String!) on ENUM_VALUE
directive @join__type(graph: join__Graph!, key: join__FieldSet, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
directive @join__field(graph: join__Graph, type: String) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
scalar join__FieldSet
scalar link__Import
enum link__Purpose { SECURITY EXECUTION }
"#;

    fn graph_count(sdl: &str) -> usize {
        // enum values carry a quoted name (`@join__graph(name: "a"`); the directive
        // *definition* line (`name: String!`) is excluded.
        sdl.matches("@join__graph(name: \"").count()
    }

    fn reparses(sdl: &str) -> bool {
        Schema::parse(sdl, "out.graphql").is_ok()
    }

    /// Keyless (root-field) graphs are reachable only at root ⇒ merging them adds no
    /// reachability ⇒ merge to one graph.
    #[test]
    fn merges_keyless_root_field_graphs() {
        let sdl = format!(
            "{PREAMBLE}
enum join__Graph {{
  A @join__graph(name: \"a\", url: \"\")
  B @join__graph(name: \"b\", url: \"\")
}}
type Query @join__type(graph: A) @join__type(graph: B) {{
  a: Int @join__field(graph: A)
  b: Int @join__field(graph: B)
}}
"
        );
        let out = consolidate_for_satisfiability(&sdl);
        assert!(reparses(&out), "consolidated output must parse:\n{out}");
        assert_eq!(
            graph_count(&out),
            1,
            "keyless root-field graphs should merge:\n{out}"
        );
        assert!(
            !out.contains("graph: B"),
            "B should be remapped away:\n{out}"
        );
    }

    /// Keyed graphs are never merged — even with an identical key signature — because that is
    /// the class of merge whose soundness depends on `@key` subtleties reviewers flagged
    /// (@external keys, @override, differing key-field types). Staying separate is always safe.
    #[test]
    fn keeps_keyed_graphs_separate() {
        let sdl = format!(
            "{PREAMBLE}
enum join__Graph {{
  A @join__graph(name: \"a\", url: \"\")
  B @join__graph(name: \"b\", url: \"\")
}}
type Query @join__type(graph: A) {{ x: T @join__field(graph: A) }}
type T @join__type(graph: A, key: \"id\") @join__type(graph: B, key: \"id\") {{
  id: ID! @join__field(graph: A) @join__field(graph: B)
}}
"
        );
        let out = consolidate_for_satisfiability(&sdl);
        assert!(reparses(&out), "consolidated output must parse:\n{out}");
        assert_eq!(
            graph_count(&out),
            2,
            "keyed graphs must stay separate even with identical key sigs:\n{out}"
        );
    }

    /// Different keys are keyed graphs too ⇒ stay separate (regression guard alongside the
    /// identical-key case).
    #[test]
    fn keeps_different_key_signatures_separate() {
        let sdl = format!(
            "{PREAMBLE}
enum join__Graph {{
  A @join__graph(name: \"a\", url: \"\")
  B @join__graph(name: \"b\", url: \"\")
}}
type Query @join__type(graph: A) {{ x: T @join__field(graph: A) }}
type T @join__type(graph: A, key: \"id\") @join__type(graph: B, key: \"code\") {{
  id: ID! @join__field(graph: A)
  code: ID! @join__field(graph: B)
}}
"
        );
        let out = consolidate_for_satisfiability(&sdl);
        assert!(reparses(&out), "consolidated output must parse:\n{out}");
        assert_eq!(
            graph_count(&out),
            2,
            "different key sigs must stay separate:\n{out}"
        );
    }

    /// A `@key(resolvable: false)` graph is *not* a real entry point, so it must never be
    /// swept into a merge (the exact `@key(resolvable: false)` hazard reviewers raised). Since
    /// it carries a key it is treated as keyed and left as its own representative — it stays
    /// separate from a genuinely-resolvable graph declaring the same key.
    #[test]
    fn resolvable_false_key_not_merged() {
        let sdl = format!(
            "{PREAMBLE}
enum join__Graph {{
  A @join__graph(name: \"a\", url: \"\")
  B @join__graph(name: \"b\", url: \"\")
}}
type Query @join__type(graph: A) {{ x: T @join__field(graph: A) }}
type T @join__type(graph: A, key: \"id\") @join__type(graph: B, key: \"id\", resolvable: false) {{
  id: ID! @join__field(graph: A) @join__field(graph: B)
}}
"
        );
        let out = consolidate_for_satisfiability(&sdl);
        assert!(reparses(&out), "consolidated output must parse:\n{out}");
        assert_eq!(
            graph_count(&out),
            2,
            "resolvable:false key graph must not merge into a resolvable one:\n{out}"
        );
    }

    /// Two keyless graphs that both declare the same field with *divergent* `@join__field`
    /// metadata (here, different `type:` overrides on a `@shareable` root field) must NOT be
    /// merged — folding them would leave two conflicting `@join__field(graph: REP)` on one field
    /// and make subgraph extraction define it twice (a spurious composition error). Regression
    /// for the divergent-return-type case.
    #[test]
    fn keeps_divergent_shared_field_graphs_separate() {
        let sdl = format!(
            "{PREAMBLE}
enum join__Graph {{
  A @join__graph(name: \"a\", url: \"\")
  B @join__graph(name: \"b\", url: \"\")
}}
type Query @join__type(graph: A) @join__type(graph: B) {{
  foo: I @join__field(graph: A, type: \"X\") @join__field(graph: B, type: \"Y\")
}}
interface I @join__type(graph: A) @join__type(graph: B) {{ name: String }}
type X implements I @join__type(graph: A) @join__implements(graph: A, interface: \"I\") {{ name: String @join__field(graph: A) }}
type Y implements I @join__type(graph: B) @join__implements(graph: B, interface: \"I\") {{ name: String @join__field(graph: B) }}
"
        );
        let out = consolidate_for_satisfiability(&sdl);
        assert!(reparses(&out), "consolidated output must parse:\n{out}");
        assert_eq!(
            graph_count(&out),
            2,
            "graphs sharing a field with divergent @join__field metadata must stay separate:\n{out}"
        );
    }

    /// A union's `= A | B` member clause must survive (AST serialization handles placement;
    /// regression from block-bots-graph@main, which the text rewriter broke).
    #[test]
    fn preserves_union_with_directives() {
        let sdl = format!(
            "{PREAMBLE}
enum join__Graph {{
  A @join__graph(name: \"a\", url: \"\")
  B @join__graph(name: \"b\", url: \"\")
}}
type Query @join__type(graph: A) {{ x: U @join__field(graph: A) }}
type X @join__type(graph: A) {{ id: ID! @join__field(graph: A) }}
type Y @join__type(graph: B) {{ id: ID! @join__field(graph: B) }}
union U @join__type(graph: A) @join__type(graph: B) @join__unionMember(graph: A, member: \"X\") @join__unionMember(graph: B, member: \"Y\") = X | Y
"
        );
        let out = consolidate_for_satisfiability(&sdl);
        assert!(
            reparses(&out),
            "consolidated output must parse (union member clause intact):\n{out}"
        );
        let union_line = out
            .lines()
            .find(|l| l.trim_start().starts_with("union U"))
            .unwrap();
        assert!(
            union_line.contains("= X | Y"),
            "union members must be preserved:\n{union_line}"
        );
    }
}
