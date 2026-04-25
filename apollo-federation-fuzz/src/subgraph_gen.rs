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
    /// Probability (0..=255) that an entity's `@key` becomes compound (more
    /// than just `id`). When fired, 1 (or rarely 2) extra non-null scalar
    /// key components are appended, e.g. `@key(fields: "id k0")` or
    /// `@key(fields: "id k0 k1")`. The compound key is uniform across every
    /// host of the entity. Probes the planner's compound-key handling for
    /// entity boundary fetches.
    pub compound_key_chance: u8,
    /// Probability (0..=255) that the Query root field `q<Entity>: <Entity>`
    /// in the entity's primary subgraph gets a `@provides(fields: "f [g]")`
    /// directive. Each named field is owned by some *other* subgraph and is
    /// marked `@external` on the primary's declaration of the entity. The
    /// planner can then resolve those fields without bouncing to the
    /// owner subgraph for that access path. Pokes a planner code path
    /// adjacent to but distinct from `@requires`.
    pub provides_chance: u8,
    /// Conditional on `interface_chance` firing: probability (0..=255) that
    /// the generated interface gets an `@interfaceObject` peer in another
    /// subgraph. The host's interface declaration becomes
    /// `interface I0 @key(fields: "id") { id: ID! }` and a different
    /// subgraph emits `type I0 @key(fields: "id") @interfaceObject { id: ID!
    /// [extra fields] }`. Composition distributes those extra fields onto
    /// every implementer in the supergraph. Targets PR #8109 territory
    /// (`__typename` on interface object types).
    pub interface_object_chance: u8,
    /// Probability (0..=255) that an entity gets one (or rarely two)
    /// inter-entity reference fields, e.g. `related: T<j>`. Each
    /// referencing subgraph emits a key-only stub of the target entity
    /// if it doesn't already host that entity. Probes the planner's
    /// entity-traversal join planning across subgraph boundaries —
    /// needed for PR #8016 territory (multi-hop `@requires`).
    pub inter_entity_ref_chance: u8,
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
            compound_key_chance: 130, // ~51% of entities get a compound key
            provides_chance: 130, // ~51% of entities with eligible fields
            interface_object_chance: 150, // ~59% of generated interfaces
            inter_entity_ref_chance: 150, // ~59% of entities get a ref
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
    /// `true` once this field appears in some entity's `provides_on_root`
    /// selection. Federation treats every `@provides` site as an additional
    /// resolver, so the original owner must mark the field `@shareable`.
    in_provides: bool,
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
    /// Extra key components beyond the always-present `id: ID!`. Each is a
    /// `(name, type_str)` pair where `type_str` is a non-null scalar (e.g.
    /// `"String!"`). Empty for a single-field `@key(fields: "id")`. When
    /// non-empty, every host emits these fields and the same compound
    /// `@key(fields: "id ...")` directive.
    extra_key_fields: Vec<(String, String)>,
    /// Optional `@provides(fields: "f [g]")` to attach to this entity's
    /// `q<Entity>: <Entity>` Query root field in `primary`. The named
    /// fields are owned by some other subgraph and marked `@external` on
    /// the primary's entity declaration so the planner can resolve them
    /// inline through this access path.
    provides_on_root: Option<String>,
    /// Inter-entity reference fields, e.g. `related: T<j>`. These are
    /// emitted alongside scalar fields in each contributing subgraph;
    /// when a subgraph emits one but doesn't already host the target
    /// entity, a key-only stub of the target is emitted in that
    /// subgraph so federation can stitch the reference.
    entity_ref_fields: Vec<EntityRefField>,
}

#[derive(Debug, Clone)]
struct EntityRefField {
    /// Field name (e.g. `r0_1` — `r` prefix to avoid collisions with
    /// scalar `f<i>_<j>`, key `k<n>`, and interface-object `io<n>`
    /// names).
    name: String,
    /// Index into `entities` of the target entity type.
    target_idx: usize,
    /// Subset of the containing entity's hosts that contribute this
    /// reference. Multi-host fields get `@shareable` like scalars do.
    hosts: Vec<usize>,
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
    /// Optional `@interfaceObject` peer. When present, `host_subgraph`'s
    /// interface declaration includes `@key(fields: "id")`, the listed
    /// peer subgraph emits `type <name> @key(fields: "id") @interfaceObject
    /// { id: ID! [extra fields] }`, and every implementing entity gets a
    /// fallback `@key(fields: "id")` so the federation rule "matching @key
    /// across the interface and its implementers" is satisfied even when
    /// the implementer's primary key is compound.
    interface_object: Option<InterfaceObjectPlan>,
}

#[derive(Debug, Clone)]
struct InterfaceObjectPlan {
    /// Subgraph hosting the `@interfaceObject` declaration. Distinct from
    /// the interface's `host_subgraph`.
    host_subgraph: usize,
    /// Extra non-key scalar fields contributed via the interface object,
    /// joined into every implementer of the interface in the supergraph.
    /// Each is a `(name, type_str)` pair; `type_str` is e.g. `"Int!"`.
    extra_fields: Vec<(String, String)>,
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
                in_provides: false,
            });
        }

        entities.push(EntityPlan {
            name: format!("T{i}"),
            hosts,
            primary,
            fields,
            extra_key_fields: Vec::new(),
            provides_on_root: None,
            entity_ref_fields: Vec::new(),
        });
    }

    // Compound `@key` augmentation. With probability `compound_key_chance`,
    // an entity gets 1 (or, half the time, 2) extra non-null scalar key
    // components appended to its key. The same compound key is declared on
    // every host. Probes compound-key handling at entity boundaries.
    for entity in entities.iter_mut() {
        if u.arbitrary::<u8>()? > cfg.compound_key_chance {
            continue;
        }
        let extra_count = if bool::arbitrary(u)? { 1 } else { 2 };
        const KEY_SCALARS: &[&str] = &["String!", "ID!", "Int!"];
        for k in 0..extra_count {
            let scalar = KEY_SCALARS[u.choose_index(KEY_SCALARS.len())?];
            entity
                .extra_key_fields
                .push((format!("k{k}"), scalar.to_string()));
        }
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

    // Inter-entity reference augmentation. For each entity T_i, with
    // probability `inter_entity_ref_chance`, append 1 (or sometimes 2)
    // reference field(s) of the form `r<i>_<n>: T<j>` for some j != i.
    // Each ref field gets a non-empty subset of T_i's hosts. Subgraphs
    // that contribute the field but don't already host T_j will emit a
    // key-only stub of T_j during emission.
    if entities.len() >= 2 {
        for i in 0..entities.len() {
            if u.arbitrary::<u8>()? > cfg.inter_entity_ref_chance {
                continue;
            }
            let want = if bool::arbitrary(u)? { 1 } else { 2 };
            for n in 0..want {
                let candidate_targets: Vec<usize> =
                    (0..entities.len()).filter(|&j| j != i).collect();
                if candidate_targets.is_empty() {
                    break;
                }
                let target_idx =
                    candidate_targets[u.choose_index(candidate_targets.len())?];
                let host_subset = sample_nonempty_subset_of(u, &entities[i].hosts)?;
                entities[i].entity_ref_fields.push(EntityRefField {
                    name: format!("r{i}_{n}"),
                    target_idx,
                    hosts: host_subset,
                });
            }
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
                interface_object: None,
            });
        }
    }

    // @interfaceObject augmentation. For each generated interface, with
    // probability `interface_object_chance`, pick a different subgraph to
    // host a `type I0 @key(fields: "id") @interfaceObject { id: ID! ... }`
    // declaration. The interface's host gains `@key(fields: "id")` and
    // every implementing entity gets a fallback `@key(fields: "id")` (in
    // addition to whatever compound key it already has) so the federation
    // rule "matching @key across the interface and its implementers"
    // holds.
    for iface in interfaces.iter_mut() {
        if u.arbitrary::<u8>()? > cfg.interface_object_chance {
            continue;
        }
        // Federation rule: a subgraph hosting `@interfaceObject I0` must
        // *not* declare any implementation types of `I0`. Even
        // implementers without an explicit `implements I0` clause in this
        // subgraph still count as implementers of the supergraph type.
        // Also: inter-entity reference stubs of an implementer (emitted
        // by another entity's `entity_ref_fields` pointing here) count
        // as a declaration of that implementer too.
        let stub_host = |s: usize, target_idx: usize| -> bool {
            entities.iter().any(|e| {
                e.entity_ref_fields
                    .iter()
                    .any(|r| r.target_idx == target_idx && r.hosts.contains(&s))
            })
        };
        let candidates: Vec<usize> = (0..subgraph_count)
            .filter(|s| {
                *s != iface.host_subgraph
                    && !iface.implementing_entities.iter().any(|&e_idx| {
                        entities[e_idx].hosts.contains(s) || stub_host(*s, e_idx)
                    })
            })
            .collect();
        if candidates.is_empty() {
            continue;
        }
        let host_subgraph = candidates[u.choose_index(candidates.len())?];
        // 1 (or sometimes 2) extra scalar fields contributed via the
        // interface object. Names use the `io` prefix to avoid colliding
        // with entity field names (`f<i>_<j>`) and key fields (`k<n>`).
        let extra_count = if bool::arbitrary(u)? { 1 } else { 2 };
        const IO_SCALARS: &[&str] = &["Int!", "String!", "Boolean!", "Float", "ID"];
        let mut extra_fields = Vec::with_capacity(extra_count);
        for k in 0..extra_count {
            let scalar = IO_SCALARS[u.choose_index(IO_SCALARS.len())?];
            extra_fields.push((format!("io{k}"), scalar.to_string()));
        }
        iface.interface_object = Some(InterfaceObjectPlan {
            host_subgraph,
            extra_fields,
        });
    }

    // @provides augmentation. For each entity, with probability
    // `provides_chance`, attach `@provides(fields: "f [g]")` to the entity's
    // `q<Entity>: <Entity>` Query root field. Each provided field is owned
    // by some subgraph other than `primary` and gets `@external` added on
    // primary's entity declaration so the planner sees a valid provide
    // site. Pokes a planner code path adjacent to but distinct from
    // `@requires`.
    //
    // Eligibility per field: hosted by exactly one subgraph (other than
    // primary), no `@override`, no entanglement that would conflict with
    // `@external` (no existing `external_in.contains(&primary)`, no
    // `requires_in` from primary).
    for entity in entities.iter_mut() {
        if u.arbitrary::<u8>()? > cfg.provides_chance {
            continue;
        }
        let primary = entity.primary;
        let candidate_idxs: Vec<usize> = entity
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                f.hosts.len() == 1
                    && f.hosts[0] != primary
                    && f.override_in.is_none()
                    && !f.external_in.contains(&primary)
                    && !f.requires_in.iter().any(|(s, _)| *s == primary)
            })
            .map(|(i, _)| i)
            .collect();
        if candidate_idxs.is_empty() {
            continue;
        }
        // 1 field most of the time, occasionally 2 to exercise multi-field
        // `@provides(fields: "f g")` selections.
        let want = if candidate_idxs.len() >= 2 && bool::arbitrary(u)? {
            2
        } else {
            1
        };
        let mut chosen_names = Vec::with_capacity(want);
        let mut remaining = candidate_idxs;
        for _ in 0..want {
            if remaining.is_empty() {
                break;
            }
            let pick = u.choose_index(remaining.len())?;
            let field_idx = remaining.remove(pick);
            entity.fields[field_idx].external_in.push(primary);
            entity.fields[field_idx].in_provides = true;
            chosen_names.push(entity.fields[field_idx].name.clone());
        }
        if !chosen_names.is_empty() {
            entity.provides_on_root = Some(chosen_names.join(" "));
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
                let provides_clause = match &e.provides_on_root {
                    Some(sel) => format!(" @provides(fields: \"{sel}\")"),
                    None => String::new(),
                };
                let _ = writeln!(sdl, "  q{}: {}{provides_clause}", e.name, e.name);
            }
            for i in &interfaces_here {
                let _ = writeln!(sdl, "  q{}: {}", i.name, i.name);
            }
            sdl.push_str("}\n\n");
        }

        // Interface declarations live in the interface-host subgraph only.
        // When an `@interfaceObject` peer exists for the interface, the
        // declaration carries `@key(fields: "id")` so the federation
        // matching-key rule is satisfied.
        for i in &interfaces_here {
            let key_clause = if i.interface_object.is_some() {
                " @key(fields: \"id\")"
            } else {
                ""
            };
            let _ = writeln!(
                sdl,
                "interface {}{key_clause} {{\n  id: ID!\n}}\n",
                i.name
            );
        }

        // `@interfaceObject` declaration: a subgraph that did not host the
        // interface itself emits a sibling `type <name> @key @interfaceObject`
        // contributing extra fields that get joined into every implementer
        // in the supergraph.
        for i in interfaces.iter() {
            if let Some(io) = &i.interface_object
                && io.host_subgraph == s
            {
                let _ = writeln!(
                    sdl,
                    "type {} @key(fields: \"id\") @interfaceObject {{",
                    i.name
                );
                sdl.push_str("  id: ID!\n");
                for (n, t) in &io.extra_fields {
                    let _ = writeln!(sdl, "  {n}: {t}");
                }
                sdl.push_str("}\n\n");
            }
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
            let mut key_fields_sel = String::from("id");
            for (n, _) in &e.extra_key_fields {
                key_fields_sel.push(' ');
                key_fields_sel.push_str(n);
            }
            // If this entity implements an interface that has an
            // `@interfaceObject` peer, the federation rule requires a
            // matching `@key(fields: "id")` on the implementer. We always
            // already emit the primary key; when it's compound (e.g.
            // `id k0`) we additionally emit a plain `@key(fields: "id")`
            // so the match holds. With a non-compound primary key the
            // primary key already matches, so no fallback is needed.
            let needs_id_fallback_key = !e.extra_key_fields.is_empty()
                && interfaces.iter().any(|i| {
                    i.interface_object.is_some()
                        && i.implementing_entities.contains(&e_idx)
                });
            let extra_id_key_clause = if needs_id_fallback_key {
                " @key(fields: \"id\")"
            } else {
                ""
            };
            let _ = writeln!(
                sdl,
                "type {}{implements_clause} @key(fields: \"{key_fields_sel}\"){extra_id_key_clause} {{",
                e.name
            );
            sdl.push_str("  id: ID!\n");
            for (n, t) in &e.extra_key_fields {
                let _ = writeln!(sdl, "  {n}: {t}");
            }
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
                } else if owned_here
                    && !field_has_override
                    && (f.hosts.len() > 1 || f.in_provides)
                {
                    // Once a field has an `@override`, post-composition there
                    // is exactly one resolver, so no host needs `@shareable`.
                    // `@provides` adds the providing subgraph as an extra
                    // resolver, so the owner must mark the field shareable.
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
            // Inter-entity reference fields. Multi-host refs get
            // @shareable like scalar fields do (federation requires
            // every resolver of a field to opt into sharing).
            for r in &e.entity_ref_fields {
                if !r.hosts.contains(&s) {
                    continue;
                }
                let shareable = if r.hosts.len() > 1 { " @shareable" } else { "" };
                let _ = writeln!(
                    sdl,
                    "  {}: {}{}",
                    r.name, entities[r.target_idx].name, shareable
                );
            }
            sdl.push_str("}\n\n");
        }

        // Key-only stubs: for any entity referenced from this subgraph
        // (via someone's `entity_ref_fields`) that this subgraph doesn't
        // itself host, emit a stub `type T_j @key(fields: "id [k0 k1]")
        // { id: ID! [k0: ... ] }` so federation can stitch the reference.
        let mut needs_stub: Vec<usize> = Vec::new();
        for e in entities.iter() {
            for r in &e.entity_ref_fields {
                if r.hosts.contains(&s)
                    && !entities[r.target_idx].hosts.contains(&s)
                    && !needs_stub.contains(&r.target_idx)
                {
                    needs_stub.push(r.target_idx);
                }
            }
        }
        needs_stub.sort_unstable();
        for j in needs_stub {
            let target = &entities[j];
            let mut key_fields_sel = String::from("id");
            for (n, _) in &target.extra_key_fields {
                key_fields_sel.push(' ');
                key_fields_sel.push_str(n);
            }
            let _ = writeln!(
                sdl,
                "type {} @key(fields: \"{key_fields_sel}\") {{",
                target.name
            );
            sdl.push_str("  id: ID!\n");
            for (n, t) in &target.extra_key_fields {
                let _ = writeln!(sdl, "  {n}: {t}");
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
