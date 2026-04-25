//! Layer 1: federated subgraph SDL generator.
//!
//! Phase C scope: a constructive generator that emits **composition-valid**
//! subgraph sets exercising `@key` + `@shareable`. Intentionally narrow:
//! - All object types are entities with single-field `id: ID!` keys.
//! - No interfaces, unions, enums, input objects, or inter-object fields yet.
//! - Built-in scalars only. (Phase E expands the directive surface.)
//!
//! The generator is driven by [`arbitrary::Unstructured`] so it plugs into
//! both proptest and libfuzzer harnesses unchanged.

use arbitrary::{Arbitrary, Result as ArbResult, Unstructured};
use std::fmt::Write as _;

/// A named subgraph SDL pair, suitable as input to [`crate::compose`].
///
/// SDL is emitted *without* a `@link` to the federation spec — the compose
/// layer calls `Subgraph::into_fed2_test_subgraph(true)` which injects the
/// standard fed2 link with all directive imports. Pre-injecting it would
/// cause `InvalidLinkDirectiveUsage: duplicate @link inclusion`.
#[derive(Debug, Clone)]
pub struct SubgraphSdl {
    pub name: String,
    pub sdl: String,
}

impl SubgraphSdl {
    pub fn new(name: impl Into<String>, body: impl AsRef<str>) -> Self {
        Self {
            name: name.into(),
            sdl: body.as_ref().to_string(),
        }
    }
}

/// A trivial two-subgraph fixture used for the Phase-A smoke test.
/// Demonstrates an entity (`User`) shared across subgraphs via `@key`.
pub fn smoke_test_fixture() -> Vec<SubgraphSdl> {
    let users = SubgraphSdl::new(
        "users",
        r#"
            type Query {
              me: User
            }

            type User @key(fields: "id") {
              id: ID!
              name: String!
            }
        "#,
    );

    let reviews = SubgraphSdl::new(
        "reviews",
        r#"
            type Query {
              latestReview: Review
            }

            type Review @key(fields: "id") {
              id: ID!
              body: String!
              author: User!
            }

            type User @key(fields: "id") {
              id: ID!
              reviews: [Review!]!
            }
        "#,
    );

    vec![users, reviews]
}

#[derive(Debug, Clone)]
pub struct GenConfig {
    pub min_subgraphs: usize,
    pub max_subgraphs: usize,
    pub min_entities: usize,
    pub max_entities: usize,
    pub max_fields_per_entity: usize,
}

impl Default for GenConfig {
    fn default() -> Self {
        Self {
            min_subgraphs: 2,
            max_subgraphs: 4,
            min_entities: 2,
            max_entities: 5,
            max_fields_per_entity: 4,
        }
    }
}

const SCALAR_TYPES: &[&str] = &["ID", "String", "Int", "Float", "Boolean"];

/// One non-key field on an entity, plus the indices of the subgraphs that
/// contribute it. If `hosts.len() > 1` the field is emitted with `@shareable`
/// in each contributing subgraph.
#[derive(Debug, Clone)]
struct EntityField {
    name: String,
    type_str: String,
    hosts: Vec<usize>,
}

#[derive(Debug, Clone)]
struct EntityPlan {
    name: String,
    /// Subgraph indices that host this entity (and therefore must declare
    /// the @key fields).
    hosts: Vec<usize>,
    /// The single subgraph that owns the Query root field for this entity.
    /// Always one of `hosts`. Keeps Query non-shareable.
    primary: usize,
    fields: Vec<EntityField>,
}

/// Produce a composition-valid set of subgraphs.
pub fn generate_federated_subgraphs(
    u: &mut Unstructured,
    cfg: &GenConfig,
) -> ArbResult<Vec<SubgraphSdl>> {
    let subgraph_count = sample_range(u, cfg.min_subgraphs, cfg.max_subgraphs)?;
    let entity_count = sample_range(u, cfg.min_entities, cfg.max_entities)?;

    let mut entities: Vec<EntityPlan> = Vec::with_capacity(entity_count);
    for i in 0..entity_count {
        let hosts = sample_nonempty_subset(u, subgraph_count)?;
        let primary = hosts[u.choose_index(hosts.len())?];
        let field_count = sample_range(u, 0, cfg.max_fields_per_entity)?;

        let mut fields = Vec::with_capacity(field_count);
        for j in 0..field_count {
            let type_str = field_type(u)?;
            let field_hosts = sample_nonempty_subset_of(u, &hosts)?;
            fields.push(EntityField {
                name: format!("f{i}_{j}"),
                type_str,
                hosts: field_hosts,
            });
        }

        entities.push(EntityPlan {
            name: format!("T{i}"),
            hosts,
            primary,
            fields,
        });
    }

    // Make sure every subgraph hosts at least one entity. If not, append the
    // subgraph to a random entity's host list.
    let mut hosted_anywhere = vec![false; subgraph_count];
    for e in &entities {
        for &h in &e.hosts {
            hosted_anywhere[h] = true;
        }
    }
    for (s, hosted) in hosted_anywhere.iter().enumerate() {
        if !hosted {
            let idx = u.choose_index(entities.len())?;
            entities[idx].hosts.push(s);
            entities[idx].hosts.sort_unstable();
            entities[idx].hosts.dedup();
        }
    }

    Ok(emit(subgraph_count, &entities))
}

fn emit(subgraph_count: usize, entities: &[EntityPlan]) -> Vec<SubgraphSdl> {
    let mut out = Vec::with_capacity(subgraph_count);
    for s in 0..subgraph_count {
        let mut sdl = String::new();

        // Query root: only entities for which this subgraph is the primary
        // host appear here; this avoids needing @shareable on Query fields.
        let primary_for: Vec<&EntityPlan> =
            entities.iter().filter(|e| e.primary == s).collect();
        if !primary_for.is_empty() {
            sdl.push_str("type Query {\n");
            for e in &primary_for {
                let _ = writeln!(sdl, "  q{}: {}", e.name, e.name);
            }
            sdl.push_str("}\n\n");
        }

        // Entity declarations: one per entity hosted by this subgraph.
        for e in entities {
            if !e.hosts.contains(&s) {
                continue;
            }
            let _ = writeln!(sdl, "type {} @key(fields: \"id\") {{", e.name);
            sdl.push_str("  id: ID!\n");
            for f in &e.fields {
                if !f.hosts.contains(&s) {
                    continue;
                }
                let shareable = if f.hosts.len() > 1 { " @shareable" } else { "" };
                let _ = writeln!(sdl, "  {}: {}{}", f.name, f.type_str, shareable);
            }
            sdl.push_str("}\n\n");
        }

        out.push(SubgraphSdl::new(format!("s{s}"), sdl));
    }
    out
}

fn field_type(u: &mut Unstructured) -> ArbResult<String> {
    let scalar = SCALAR_TYPES[u.choose_index(SCALAR_TYPES.len())?];
    let nonnull = bool::arbitrary(u)?;
    Ok(if nonnull {
        format!("{scalar}!")
    } else {
        scalar.to_string()
    })
}

fn sample_range(u: &mut Unstructured, lo: usize, hi: usize) -> ArbResult<usize> {
    if lo >= hi {
        return Ok(lo);
    }
    u.int_in_range(lo..=hi)
}

fn sample_nonempty_subset(u: &mut Unstructured, n: usize) -> ArbResult<Vec<usize>> {
    let universe: Vec<usize> = (0..n).collect();
    sample_nonempty_subset_of(u, &universe)
}

fn sample_nonempty_subset_of(u: &mut Unstructured, universe: &[usize]) -> ArbResult<Vec<usize>> {
    debug_assert!(!universe.is_empty());
    let mut chosen: Vec<usize> = Vec::new();
    for &x in universe {
        if bool::arbitrary(u)? {
            chosen.push(x);
        }
    }
    if chosen.is_empty() {
        chosen.push(universe[u.choose_index(universe.len())?]);
    }
    chosen.sort_unstable();
    chosen.dedup();
    Ok(chosen)
}
