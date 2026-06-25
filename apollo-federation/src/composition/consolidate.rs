//! Satisfiability-only consolidation of connector synthetic subgraphs.
//!
//! Connector expansion (`expand_connectors`) turns each `@connect` into its own synthetic
//! subgraph so satisfiability can reuse the existing subgraph-based machinery. On large
//! connector supergraphs this explodes into thousands of `@join__graph`s, and
//! `validate_satisfiability` over that expansion is the dominant memory/time cost (it
//! builds a federated query graph with a node per type x subgraph, scaling super-linearly).
//!
//! This transform merges `@join__graph`s that share an **identical resolvable-key
//! signature** (the set of `@join__type(T, key: K)` entries they declare) into one
//! representative graph, purely for the satisfiability check. The expanded supergraph the
//! router executes against is **not** affected — this rewrites only the schema handed to
//! `validate_satisfiability`.
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
//! reachability, never remove it, so a merge can only ever *mask* a satisfiability error
//! (false pass), never invent one. A merge is reachability-*preserving* exactly when the
//! merged members were already mutually reachable. Two graphs with an identical resolvable
//! -key signature are mutually-interchangeable entry points (each enterable by exactly the
//! same keys), so co-locating their fields adds nothing — the verdict is preserved.
//! Different-key or cross-type merges are *not* preserving and are deliberately excluded, as
//! is merging across spec-link scopes (a graph's `@join__directive(name: "link")` membership,
//! e.g. connect v0.2 vs v0.3) — that would make the representative link the same feature twice.
//! The grouping key is therefore (resolvable-key signature, link scope).

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

/// Rewrite an expanded supergraph SDL, merging `@join__graph`s that share an identical
/// resolvable-key signature. Returns the consolidated SDL (re-parseable as a supergraph).
/// Fails open: if the input can't be parsed, returns it unchanged so satisfiability still
/// runs on the full expansion.
pub(super) fn consolidate_for_satisfiability(expanded_sdl: &str) -> String {
    let mut schema = match Schema::parse(expanded_sdl, "expanded.graphql") {
        Ok(s) => s,
        Err(_) => return expanded_sdl.to_string(),
    };

    // --- pass 1: per-graph resolvable-key signature + spec-link scope ---
    // A graph's key signature is the set of (type, key) it can be entered by. Its link scope is
    // the set of `@join__directive(name: "link", args)` entries it belongs to (e.g. connect
    // v0.2 vs v0.3) — merging across link scopes would make the representative link the same
    // feature twice. Two graphs merge only if both match: interchangeable entry points within
    // one link scope.
    let mut keyed_sig: HashMap<Name, BTreeSet<(String, String)>> = HashMap::new();
    for (type_name, ty) in &schema.types {
        for d in type_directives(ty)
            .iter()
            .filter(|d| d.name == name!("join__type"))
        {
            if let (Some(graph), Some(key)) = (arg_enum(d, "graph"), arg_str(d, "key")) {
                keyed_sig
                    .entry(graph.clone())
                    .or_default()
                    .insert((type_name.to_string(), key.to_string()));
            }
        }
    }

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

    let all_graphs: Vec<Name> = match schema.types.get(&name!("join__Graph")) {
        Some(ExtendedType::Enum(e)) => e.values.keys().cloned().collect(),
        _ => return expanded_sdl.to_string(), // not a connector supergraph; nothing to do
    };

    // --- group by (key signature, link scope); representative = smallest member ---
    type GroupKey = (BTreeSet<(String, String)>, BTreeSet<String>);
    let mut groups: HashMap<GroupKey, Vec<Name>> = HashMap::new();
    for g in &all_graphs {
        let sig = keyed_sig.get(g).cloned().unwrap_or_default();
        let scopes = link_scope.get(g).cloned().unwrap_or_default();
        groups.entry((sig, scopes)).or_default().push(g.clone());
    }
    let mut sym2rep: HashMap<Name, Name> = HashMap::new();
    let mut reps: HashSet<Name> = HashSet::new();
    for members in groups.values() {
        let rep = members.iter().min().cloned().unwrap();
        reps.insert(rep.clone());
        sym2rep.extend(members.iter().map(|m| (m.clone(), rep.clone())));
    }
    if reps.len() == all_graphs.len() {
        return expanded_sdl.to_string(); // nothing merges
    }

    // --- pass 2: remap graph references + prune the join__Graph enum ---
    let join_graph = name!("join__Graph");
    for (type_name, ty) in schema.types.iter_mut() {
        match ty {
            ExtendedType::Scalar(t) => {
                process_schema_dl(&mut t.make_mut().directives, &sym2rep);
            }
            ExtendedType::Object(t) => {
                let t = t.make_mut();
                process_schema_dl(&mut t.directives, &sym2rep);
                for (_, f) in t.fields.iter_mut() {
                    process_ast_dl(&mut f.make_mut().directives, &sym2rep);
                }
            }
            ExtendedType::Interface(t) => {
                let t = t.make_mut();
                process_schema_dl(&mut t.directives, &sym2rep);
                for (_, f) in t.fields.iter_mut() {
                    process_ast_dl(&mut f.make_mut().directives, &sym2rep);
                }
            }
            ExtendedType::Union(t) => {
                process_schema_dl(&mut t.make_mut().directives, &sym2rep);
            }
            ExtendedType::InputObject(t) => {
                let t = t.make_mut();
                process_schema_dl(&mut t.directives, &sym2rep);
                for (_, f) in t.fields.iter_mut() {
                    process_ast_dl(&mut f.make_mut().directives, &sym2rep);
                }
            }
            ExtendedType::Enum(t) => {
                let t = t.make_mut();
                process_schema_dl(&mut t.directives, &sym2rep);
                if *type_name == join_graph {
                    t.values.retain(|k, _| reps.contains(k));
                } else {
                    for (_, v) in t.values.iter_mut() {
                        process_ast_dl(&mut v.make_mut().directives, &sym2rep);
                    }
                }
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

/// Return a copy of `d` with any `graph:` enum arg and `graphs: [...]` list arg remapped to
/// representatives (the latter deduped, since the remap can repeat a representative).
fn remapped_directive(d: &Directive, sym2rep: &HashMap<Name, Name>) -> Directive {
    let mut nd = d.clone();
    for arg in nd.arguments.iter_mut() {
        match arg.name.as_str() {
            "graph" => {
                if let Some(g) = arg.value.as_enum().cloned()
                    && let Some(rep) = sym2rep.get(&g).filter(|rep| **rep != g)
                {
                    arg.make_mut().value = Value::Enum(rep.clone()).into();
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

/// Remap and fold byte-identical duplicates in a schema-level (`Component`) directive list.
fn process_schema_dl(dl: &mut DirectiveList, sym2rep: &HashMap<Name, Name>) {
    let mut out = DirectiveList::new();
    let mut seen = HashSet::new();
    for comp in dl.iter() {
        let nd = remapped_directive(comp, sym2rep);
        if seen.insert(nd.to_string()) {
            out.push(Component::new(nd));
        }
    }
    *dl = out;
}

/// Remap and fold byte-identical duplicates in an AST (`Node`) directive list.
fn process_ast_dl(dl: &mut ast::DirectiveList, sym2rep: &HashMap<Name, Name>) {
    let mut out = ast::DirectiveList::new();
    let mut seen = HashSet::new();
    for d in dl.iter() {
        let nd = remapped_directive(d, sym2rep);
        if seen.insert(nd.to_string()) {
            out.push(Node::new(nd));
        }
    }
    *dl = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREAMBLE: &str = r#"
schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/join/v0.5", for: EXECUTION) {
  query: Query
}
directive @join__graph(name: String!, url: String!) on ENUM_VALUE
directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
directive @join__field(graph: join__Graph) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
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

    /// Same resolvable-key signature ⇒ interchangeable entry points ⇒ merge to one graph.
    #[test]
    fn merges_same_key_signature() {
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
        assert_eq!(graph_count(&out), 1, "same key sig should merge:\n{out}");
        assert!(
            !out.contains("graph: B"),
            "B should be remapped away:\n{out}"
        );
    }

    /// Different keys ⇒ not mutually reachable ⇒ must stay separate (else errors get masked).
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
