//! Layer 1: federated subgraph SDL generator.
//!
//! Current scope: composition-valid subgraph sets exercising `@key`,
//! `@shareable`, `@requires`, and `@external`. Still narrow:
//! - All object types are entities with single-field `id: ID!` keys.
//! - No interfaces, unions, enums, input objects, or inter-object fields.
//! - Built-in scalars only.
//! - `@override`, `@provides`, `@interfaceObject` not yet emitted.
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
    /// Probability (0..=255) that an entity hosted by ≥2 subgraphs gets a
    /// `@requires` link added. Set to 0 to disable.
    pub requires_chance: u8,
    /// Probability (0..=255) that the generated subgraph set declares a
    /// non-default name for the root query type (e.g. `RootQuery0`). All
    /// subgraphs in a generated set agree on the chosen name. Set to 0 to
    /// always use `Query`. Pokes at PR #7580: planner used to emit
    /// `... on Query` even when the root was renamed.
    pub renamed_root_chance: u8,
}

impl Default for GenConfig {
    fn default() -> Self {
        Self {
            min_subgraphs: 2,
            max_subgraphs: 4,
            min_entities: 2,
            max_entities: 6,
            max_fields_per_entity: 5,
            requires_chance: 200, // ~78% of eligible entities
            renamed_root_chance: 80, // ~31% of generated sets
        }
    }
}

const SCALAR_TYPES: &[&str] = &["ID", "String", "Int", "Float", "Boolean"];

/// One non-key field on an entity, plus the indices of the subgraphs that
/// contribute it. If `hosts.len() > 1` the field is emitted with `@shareable`
/// in each contributing subgraph.
///
/// Federation extras:
/// - `external_in`: subgraphs in which this field is *also* declared, but
///   only as an `@external` stub (it isn't owned there). Driven by another
///   field's `@requires(fields: <self.name>)` in that subgraph.
/// - `requires_in`: per-subgraph `@requires` directive. The string is the
///   selection that goes inside `@requires(fields: "<...>")`.
#[derive(Debug, Clone)]
struct EntityField {
    name: String,
    type_str: String,
    hosts: Vec<usize>,
    external_in: Vec<usize>,
    requires_in: Vec<(usize, String)>,
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

    // Pick a root query type name. Renaming exercises PR #7580 territory.
    let query_root_name: String = if cfg.renamed_root_chance > 0
        && u8::arbitrary(u)? < cfg.renamed_root_chance
    {
        let pool = ["RootQuery", "MyQuery", "Q", "GraphQuery"];
        pool[u.choose_index(pool.len())?].to_string()
    } else {
        "Query".to_string()
    };

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
                external_in: Vec::new(),
                requires_in: Vec::new(),
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

    // @requires augmentation. For each multi-host entity, with probability
    // `requires_chance/256`, attempt to wire up a `@requires` link between
    // two of its non-key fields living in different exclusive subgraphs.
    for entity in entities.iter_mut() {
        if entity.hosts.len() < 2 {
            continue;
        }
        if u.arbitrary::<u8>()? > cfg.requires_chance {
            continue;
        }

        // Candidates: fields hosted by exactly one subgraph (exclusive).
        // Group them by their host so we can pick a (provider, requirer)
        // pair living in different subgraphs.
        let exclusive_fields: Vec<usize> = entity
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| f.hosts.len() == 1)
            .map(|(i, _)| i)
            .collect();
        if exclusive_fields.len() < 2 {
            continue;
        }

        let provider_idx = exclusive_fields[u.choose_index(exclusive_fields.len())?];
        let provider_host = entity.fields[provider_idx].hosts[0];

        let requirer_candidates: Vec<usize> = exclusive_fields
            .iter()
            .copied()
            .filter(|&i| i != provider_idx && entity.fields[i].hosts[0] != provider_host)
            .collect();
        if requirer_candidates.is_empty() {
            continue;
        }

        let requirer_idx = requirer_candidates[u.choose_index(requirer_candidates.len())?];
        let requirer_host = entity.fields[requirer_idx].hosts[0];

        // Apply the link: provider becomes @external in requirer's subgraph;
        // requirer gets @requires(fields: "<provider.name>") in its subgraph.
        let provider_name = entity.fields[provider_idx].name.clone();
        entity.fields[provider_idx].external_in.push(requirer_host);
        entity.fields[requirer_idx]
            .requires_in
            .push((requirer_host, provider_name));
    }

    Ok(emit(subgraph_count, &entities, &query_root_name))
}

fn emit(subgraph_count: usize, entities: &[EntityPlan], query_root_name: &str) -> Vec<SubgraphSdl> {
    let mut out = Vec::with_capacity(subgraph_count);
    for s in 0..subgraph_count {
        let mut sdl = String::new();

        // Query root: only entities for which this subgraph is the primary
        // host appear here; this avoids needing @shareable on Query fields.
        let primary_for: Vec<&EntityPlan> =
            entities.iter().filter(|e| e.primary == s).collect();
        if !primary_for.is_empty() {
            // The `schema { query: <Name> }` declaration only makes sense
            // when this subgraph actually defines the root type. Subgraphs
            // without root fields skip both lines and the planner sees them
            // as having no root contribution. (Per the federation spec,
            // root type names are per-subgraph and merged into the
            // supergraph's `Query`.)
            if query_root_name != "Query" {
                let _ = writeln!(sdl, "schema {{\n  query: {query_root_name}\n}}\n");
            }
            let _ = writeln!(sdl, "type {query_root_name} {{");
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
                let owned_here = f.hosts.contains(&s);
                let external_here = !owned_here && f.external_in.contains(&s);
                if !owned_here && !external_here {
                    continue;
                }

                // Build directive suffix.
                let mut directives = String::new();
                if owned_here && f.hosts.len() > 1 {
                    directives.push_str(" @shareable");
                }
                if external_here {
                    directives.push_str(" @external");
                }
                if let Some((_, sel)) = f
                    .requires_in
                    .iter()
                    .find(|(sub, _)| *sub == s && owned_here)
                {
                    let _ = write!(directives, " @requires(fields: \"{sel}\")");
                }
                let _ = writeln!(sdl, "  {}: {}{}", f.name, f.type_str, directives);
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
