//! Layer 1: federated subgraph SDL generator.
//!
//! Current scope: composition-valid subgraph sets exercising `@key`,
//! `@shareable`, `@requires`, `@external`, and `@override` (plus optional
//! progressive labels). Still narrow:
//! - All object types are entities with single-field `id: ID!` keys.
//! - No interfaces, unions, enums, input objects, or inter-object fields.
//! - Built-in scalars only.
//! - `@provides`, `@interfaceObject` not yet emitted.
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
    /// Probability (0..=255) that an eligible 2-host field gets an
    /// `@override(from:)` directive transferring ownership. Eligibility =
    /// non-key field hosted by exactly 2 subgraphs with no `@requires` /
    /// `@external` / `@provides` entanglement and no prior `@override`.
    /// Pokes at PR #7929 territory.
    pub override_chance: u8,
    /// Conditional on `override_chance` firing: probability (0..=255) that
    /// the emitted `@override` is "progressive" — gets a `label:` argument
    /// (`"percent(N)"`). Requires federation v2.7+ in the supergraph.
    pub progressive_override_chance: u8,
    /// Probability (0..=255) that the generated subgraph set declares an
    /// interface (a single `interface I0 { id: ID! }` plus 1..=3 of one
    /// subgraph's primary entities marked `implements I0`, plus a
    /// `qI0: I0` root field). Opens up FED-505 (`@skip`/`@include` over
    /// interfaces) and PR #7929 (progressive `@override` on interface
    /// implementations). Set to 0 to disable.
    pub interface_chance: u8,
    /// Conditional on `requires_chance` firing: probability (0..=255) that
    /// the emitted `@requires` pulls in a *second* provider field owned by
    /// yet another subgraph, producing `@requires(fields: "f g")` instead
    /// of `@requires(fields: "f")`. Pokes at PR #8016 territory: assembly
    /// of multiple `@external` inputs for a single `@requires` site.
    pub multi_field_requires_chance: u8,
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
            override_chance: 160, // ~62% of eligible 2-host fields
            progressive_override_chance: 90, // ~35% of overrides get a label
            interface_chance: 130, // ~51% of generated sets
            multi_field_requires_chance: 150, // ~59% of @requires sites
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
/// - `override_in`: at most one `(new_owner_subgraph_index, "from"_subgraph_name,
///   optional_label)` triple. The subgraph at `new_owner_subgraph_index` emits
///   `@override(from: "<from>", [label: "<label>"])`; the `from` subgraph
///   keeps its declaration but neither side gets `@shareable`. Federation
///   forbids more than one `@override` per field.
#[derive(Debug, Clone)]
struct EntityField {
    name: String,
    type_str: String,
    hosts: Vec<usize>,
    external_in: Vec<usize>,
    requires_in: Vec<(usize, String)>,
    override_in: Option<(usize, String, Option<String>)>,
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

/// One generated interface, declared in a single subgraph. Implementing
/// entities are marked `implements I` only in that subgraph; other
/// subgraphs hosting the same entity types are unaware of the interface
/// (federation handles cross-subgraph stitching via `@join__implements`).
///
/// The interface only declares `id: ID!` so we don't have to add new fields
/// to implementers — every entity already has that key field.
#[derive(Debug, Clone)]
struct InterfacePlan {
    name: String,
    host_subgraph: usize,
    /// Indices into the entities list. All entries must have
    /// `host_subgraph` in their `hosts`.
    implementing_entities: Vec<usize>,
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
                override_in: None,
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

        // First provider: becomes @external in requirer's subgraph.
        let mut provider_names = vec![entity.fields[provider_idx].name.clone()];
        entity.fields[provider_idx].external_in.push(requirer_host);

        // Optionally add a second provider field, owned by yet another
        // subgraph, to produce a multi-field `@requires(fields: "f g")`.
        // Eligibility: a different exclusive field owned by a third host
        // that's neither the first provider's nor the requirer's.
        if u.arbitrary::<u8>()? < cfg.multi_field_requires_chance {
            let second_provider_candidates: Vec<usize> = exclusive_fields
                .iter()
                .copied()
                .filter(|&i| {
                    i != provider_idx
                        && i != requirer_idx
                        && entity.fields[i].hosts[0] != provider_host
                        && entity.fields[i].hosts[0] != requirer_host
                })
                .collect();
            if !second_provider_candidates.is_empty() {
                let second_provider_idx =
                    second_provider_candidates[u.choose_index(second_provider_candidates.len())?];
                provider_names.push(entity.fields[second_provider_idx].name.clone());
                entity.fields[second_provider_idx]
                    .external_in
                    .push(requirer_host);
            }
        }

        // Emit the (possibly multi-field) @requires link on the requirer.
        let requires_str = provider_names.join(" ");
        entity.fields[requirer_idx]
            .requires_in
            .push((requirer_host, requires_str));
    }

    // @override augmentation. Pokes at PR #7929 territory.
    //
    // Federation rules we honour:
    // - At most one `@override` per field across all subgraphs.
    // - Cannot @override "from self" — `from` must be a different subgraph.
    // - Cannot combine with `@external` / `@requires` / `@provides` on the
    //   same field. We satisfy this by only picking fields with empty
    //   `external_in` / `requires_in` (we don't generate `@provides` yet).
    // - @key fields are off-limits (we only override generated non-key fields).
    //
    // Restricting candidates to fields hosted by exactly 2 subgraphs keeps
    // the rule "no other host needs @shareable post-override" trivially
    // satisfied: there *are* no other hosts.
    for entity in entities.iter_mut() {
        if u.arbitrary::<u8>()? > cfg.override_chance {
            continue;
        }
        let candidates: Vec<usize> = entity
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                f.hosts.len() == 2
                    && f.external_in.is_empty()
                    && f.requires_in.is_empty()
                    && f.override_in.is_none()
            })
            .map(|(i, _)| i)
            .collect();
        if candidates.is_empty() {
            continue;
        }
        let field_idx = candidates[u.choose_index(candidates.len())?];
        let hosts = &entity.fields[field_idx].hosts;
        let new_owner_pos = u.choose_index(2)?;
        let new_owner = hosts[new_owner_pos];
        let from_subgraph = hosts[1 - new_owner_pos];
        let from_name = format!("s{from_subgraph}");

        let label = if u.arbitrary::<u8>()? < cfg.progressive_override_chance {
            // `percent(N)` is the simplest progressive-override label that
            // composition will accept. Choose 0..=100 so we hit edges.
            let pct = u.int_in_range(0..=100u32)?;
            Some(format!("percent({pct})"))
        } else {
            None
        };
        entity.fields[field_idx].override_in = Some((new_owner, from_name, label));
    }

    // Interface augmentation. Pokes at FED-505 + PR #7929 territory.
    //
    // Strategy: declare a single interface in one subgraph and mark a
    // small set of that subgraph's hosted entities as `implements I0`.
    // The interface only declares `id: ID!`, which every entity already
    // has as its @key — no new field surgery on implementers needed. A
    // root field `qI0: I0` in the interface-host subgraph gives the
    // operation generator something to query, naturally producing
    // `... on T0 { ... }` inline fragments.
    let mut interfaces: Vec<InterfacePlan> = Vec::new();
    if u.arbitrary::<u8>()? < cfg.interface_chance {
        // Pick a host subgraph with at least one hosted entity.
        let candidate_hosts: Vec<usize> = (0..subgraph_count)
            .filter(|&s| entities.iter().any(|e| e.hosts.contains(&s)))
            .collect();
        if !candidate_hosts.is_empty() {
            let host = candidate_hosts[u.choose_index(candidate_hosts.len())?];
            let hosted_entity_indices: Vec<usize> = entities
                .iter()
                .enumerate()
                .filter(|(_, e)| e.hosts.contains(&host))
                .map(|(i, _)| i)
                .collect();
            // 1..=min(3, hosted) implementers. Picking the *first* N keeps
            // the choice deterministic given the same Unstructured stream.
            let max_impls = hosted_entity_indices.len().min(3);
            let n = u.int_in_range(1..=max_impls)?;
            let implementing_entities: Vec<usize> =
                hosted_entity_indices.iter().take(n).copied().collect();
            interfaces.push(InterfacePlan {
                name: "I0".to_string(),
                host_subgraph: host,
                implementing_entities,
            });
        }
    }

    Ok(emit(subgraph_count, &entities, &query_root_name, &interfaces))
}

fn emit(
    subgraph_count: usize,
    entities: &[EntityPlan],
    query_root_name: &str,
    interfaces: &[InterfacePlan],
) -> Vec<SubgraphSdl> {
    let mut out = Vec::with_capacity(subgraph_count);
    for s in 0..subgraph_count {
        let mut sdl = String::new();

        // Interfaces hosted by this subgraph emit a `qI0: I0` root field.
        let interfaces_here: Vec<&InterfacePlan> =
            interfaces.iter().filter(|i| i.host_subgraph == s).collect();

        // Query root: entities for which this subgraph is the primary host,
        // plus any interface-roots hosted here.
        let primary_for: Vec<&EntityPlan> =
            entities.iter().filter(|e| e.primary == s).collect();
        let need_root = !primary_for.is_empty() || !interfaces_here.is_empty();
        if need_root {
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
            for i in &interfaces_here {
                let _ = writeln!(sdl, "  q{}: {}", i.name, i.name);
            }
            sdl.push_str("}\n\n");
        }

        // Interface declarations live in the interface-host subgraph only.
        for i in &interfaces_here {
            let _ = writeln!(sdl, "interface {} {{\n  id: ID!\n}}\n", i.name);
        }

        // Entity declarations: one per entity hosted by this subgraph.
        for (e_idx, e) in entities.iter().enumerate() {
            if !e.hosts.contains(&s) {
                continue;
            }
            // Interfaces this entity implements *in this subgraph*.
            let implements: Vec<&str> = interfaces
                .iter()
                .filter(|i| i.host_subgraph == s && i.implementing_entities.contains(&e_idx))
                .map(|i| i.name.as_str())
                .collect();
            let implements_clause = if implements.is_empty() {
                String::new()
            } else {
                format!(" implements {}", implements.join(" & "))
            };
            let _ = writeln!(
                sdl,
                "type {}{implements_clause} @key(fields: \"id\") {{",
                e.name
            );
            sdl.push_str("  id: ID!\n");
            for f in &e.fields {
                let owned_here = f.hosts.contains(&s);
                let external_here = !owned_here && f.external_in.contains(&s);
                if !owned_here && !external_here {
                    continue;
                }

                // Build directive suffix.
                let mut directives = String::new();
                let is_override_owner = f
                    .override_in
                    .as_ref()
                    .is_some_and(|(idx, _, _)| *idx == s);
                let field_has_override = f.override_in.is_some();
                if is_override_owner {
                    let (_, from, label) = f.override_in.as_ref().unwrap();
                    match label {
                        Some(l) => {
                            let _ = write!(
                                directives,
                                " @override(from: \"{from}\", label: \"{l}\")"
                            );
                        }
                        None => {
                            let _ = write!(directives, " @override(from: \"{from}\")");
                        }
                    }
                } else if owned_here && f.hosts.len() > 1 && !field_has_override {
                    // Once a field has an `@override`, post-composition there
                    // is exactly one resolver, so no host needs `@shareable`.
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
