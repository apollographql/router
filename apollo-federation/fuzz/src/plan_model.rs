//! The bounded byte grammar for the query-plan lane, implemented here and again in
//! `lean/QueryPlanCheckerOracle.lean`.
//!
//! Nothing is serialized between the two sides: both decode the *same bytes* with their own copy
//! of this grammar, as the inclusion lane does. [`schema_digest`] is what guards the two copies of
//! the fixture against drift.
//!
//! # Why a case is an operation plus a perturbation
//!
//! A plan drawn freely from bytes is almost never correct for the operation beside it, and a lane
//! whose every case is rejected exercises only the rejection paths. So a case is decoded in three
//! steps: an operation, the plan a miniature planner gives that operation, and one perturbation of
//! that plan. The unperturbed plan is correct by construction, and each perturbation breaks one
//! named thing, so the lane covers acceptance and rejection and says which is expected.
//!
//! The miniature planner is not the real one — it is the smallest thing that produces the shape
//! the real planner produces for this fixture, which the `it_handles_multiple_requires_*` snapshot
//! shows. The planner-driven lane in the runner is what tests real plans.

use std::sync::Arc;

use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_federation::query_plan::requires_selection;
use apollo_federation::query_plan::serializable_document::SerializableDocument;
use apollo_federation::query_plan::DeferNode;
use apollo_federation::query_plan::DeferredDeferBlock;
use apollo_federation::query_plan::DeferredDependency;
use apollo_federation::query_plan::FetchDataKeyRenamer;
use apollo_federation::query_plan::FetchDataPathElement;
use apollo_federation::query_plan::FetchDataRewrite;
use apollo_federation::query_plan::FetchDataValueSetter;
use apollo_federation::query_plan::FetchNode;
use apollo_federation::query_plan::FlattenNode;
use apollo_federation::query_plan::ParallelNode;
use apollo_federation::query_plan::PlanNode;
use apollo_federation::query_plan::PrimaryDeferBlock;
use apollo_federation::query_plan::QueryPlan;
use apollo_federation::query_plan::SequenceNode;
use apollo_federation::query_plan::TopLevelPlanNode;
use apollo_federation::schema::ValidFederationSchema;

//==================================================================================================
// The fixture, as tables
//==================================================================================================

/// The entity types of the fixture, in the order the grammar indexes them. `T1` has no `@key`, so
/// no entity fetch can be made against it — a plan that tries is wrong, and the grammar can say so.
pub const OBJECT_TYPES: [&str; 3] = ["T1", "T2", "T3"];

/// The types an entity fetch may ask about: those carrying a resolvable `@key` in Subgraph2.
pub const ENTITY_TYPES: [&str; 2] = ["T2", "T3"];

/// The `@key` field set of every entity type here.
pub const KEY_FIELDS: &str = "id";

/// The `@requires` field set of `Tn.g` in Subgraph2.
pub const REQUIRES_FIELDS: &str = "f";

/// Boolean variables the grammar may guard a selection with.
pub const VARIABLES: [&str; 2] = ["v0", "v1"];

/// A canonical rendering of everything the two sides must agree on, so the oracle can prove it
/// holds the same fixture. Compared before any case runs.
/// The types the digest reports field declarations for, in a fixed order.
pub const DIGEST_TYPES: [&str; 7] = ["Query", "C", "I", "T1", "T2", "T3", "U"];

/// The subgraph types the digest reports, where a subgraph declares a type differently from the
/// supergraph: Subgraph1's `U` lacks `T3`, and Subgraph3's `I` is an object, an interface object.
pub const DIGEST_SUBGRAPH_TYPES: [(&str, &str); 2] = [("Subgraph1", "U"), ("Subgraph3", "I")];

/// A canonical rendering of everything the two sides must agree on, so the oracle can prove it
/// holds the same fixture. Compared before any case runs.
///
/// The field declarations are read from the real supergraph schema rather than from constants
/// here, so that a drift between the checked-in SDL and the oracle's hand-written tables is
/// caught. `__typename` is listed explicitly: the Lean side has to declare it, its `Schema` not
/// modelling introspection, and apollo-compiler knows it natively — so it is part of what the two
/// must agree on even though only one of them writes it down. Wrappers are dropped, neither
/// checker looking past a field's named type.
pub fn schema_digest(fixture: &crate::plan_fixture::Fixture) -> String {
    let schema = fixture.supergraph_schema();
    let objects = OBJECT_TYPES.join(" ");
    let entities = ENTITY_TYPES.join(" ");
    let variables = VARIABLES.join(" ");
    let selections: Vec<String> = (0..SELECTION_SLOTS).map(selection_source).collect();
    let fields: Vec<String> = DIGEST_TYPES
        .iter()
        .map(|type_name| render_type_fields(schema, type_name))
        .collect();
    let subgraph_fields: Vec<String> = DIGEST_SUBGRAPH_TYPES
        .iter()
        .map(|(subgraph_name, type_name)| {
            let subgraph = fixture
                .subgraph_schemas()
                .get(*subgraph_name)
                .expect("a fixture subgraph");
            format!(
                "{subgraph_name}.{}",
                render_type_fields(subgraph, type_name)
            )
        })
        .collect();
    format!(
        "objects: {objects} | entities: {entities} | key: {KEY_FIELDS} | \
         requires: {REQUIRES_FIELDS} | variables: {variables} | selections: {} | fields: {} | \
         subgraphs: {}",
        selections.join(" "),
        fields.join(" "),
        subgraph_fields.join(" ")
    )
}

fn render_type_fields(schema: &ValidFederationSchema, type_name: &str) -> String {
    let declared = schema.schema().types.get(type_name);
    let mut fields: Vec<String> = match declared {
        Some(ExtendedType::Object(ty)) => ty
            .fields
            .iter()
            .map(|(name, field)| format!("{name}:{}", field.ty.inner_named_type()))
            .collect(),
        Some(ExtendedType::Interface(ty)) => ty
            .fields
            .iter()
            .map(|(name, field)| format!("{name}:{}", field.ty.inner_named_type()))
            .collect(),
        // A union declares members rather than fields.
        Some(ExtendedType::Union(ty)) => {
            let mut members: Vec<String> =
                ty.members.iter().map(|member| member.to_string()).collect();
            members.sort();
            return format!("{type_name}={}", members.join("|"));
        }
        _ => Vec::new(),
    };
    fields.push("__typename:String".to_string());
    fields.sort();
    format!("{type_name}{{ {} }}", fields.join(" "))
}

//==================================================================================================
// The operation
//==================================================================================================

/// How many distinct selections the grammar can put under `is`.
pub const SELECTION_SLOTS: u8 = 12;

/// The most selections one operation carries. Beyond three the plans stop differing in kind.
pub const MAX_SELECTIONS: usize = 3;

/// One selection under `is`, by slot.
///
/// `g` never appears unguarded by a type condition: on `T1` it is resolved by Subgraph1 and on
/// `T2`/`T3` by Subgraph2, so an unguarded `g` would make the planner type-explode and the
/// miniature planner below would stop resembling it.
///
/// The last four reach the rest of the fixture: `peer { f }` is resolved inside Subgraph2's
/// `@provides` window; `u { ... on T3 { id } }` asks for a member Subgraph1, which resolves `u`,
/// never returns; `u { ... on T2 { id } }` for one it does; and `h` is resolved by Subgraph3's
/// interface object.
fn selection_source(slot: u8) -> String {
    match slot % SELECTION_SLOTS {
        0 => "id".to_string(),
        1 => "f".to_string(),
        2 => "__typename".to_string(),
        3 => "... on T1 { g }".to_string(),
        4 => "... on T2 { g }".to_string(),
        5 => "... on T3 { g }".to_string(),
        6 => "... on T2 { f }".to_string(),
        7 => "... on T3 { id }".to_string(),
        8 => "... on T2 { peer { f } }".to_string(),
        9 => "u { ... on T3 { id } }".to_string(),
        10 => "u { ... on T2 { id } }".to_string(),
        _ => "h".to_string(),
    }
}

/// What the base fetch selects for a slot it resolves: the slot as written, but for the member
/// Subgraph1's `U` lacks, which it cannot even name. `None` for a slot another subgraph resolves.
fn fetched_source(slot: u8) -> Option<String> {
    match slot % SELECTION_SLOTS {
        9 => Some("u { __typename }".to_string()),
        11 => None,
        other => slot_needs_entity_fetch(other)
            .is_none()
            .then(|| selection_source(other)),
    }
}

/// The slot resolved by Subgraph3's interface object.
const INTERFACE_OBJECT_SLOT: u8 = 11;

/// Which entity type a slot needs an entity fetch for, if any.
fn slot_needs_entity_fetch(slot: u8) -> Option<&'static str> {
    match slot % SELECTION_SLOTS {
        4 | 8 => Some("T2"),
        5 => Some("T3"),
        _ => None,
    }
}

/// One decoded case: what the client asked, and what plan is offered for it.
#[derive(Debug, Clone)]
pub struct Case {
    /// The slots the operation selects, deduplicated and in first-use order.
    pub slots: Vec<u8>,
    /// The variable guarding every entity-fetch-needing selection, if any.
    pub guard: Option<&'static str>,
    /// Which named breakage was applied to the canonical plan.
    pub perturbation: Perturbation,
}

/// The named ways the canonical plan is broken. `None` leaves a plan that is correct by
/// construction, which is what makes the lane test acceptance as well as rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Perturbation {
    /// The canonical plan, unbroken.
    None,
    /// The root fetch stops fetching `f`, which the entity fetch's `@requires` demands.
    DropRequiredField,
    /// One `requires` entry is dropped, leaving an entity case no entry covers.
    DropRequiresEntry,
    /// One entity case is dropped, leaving a `g` the plan never fetches.
    DropEntityCase,
    /// The `requires` entries name `f` where the subgraph's `@key` names `id`.
    WrongKeyField,
    /// The flatten path names a key the plan never fetched, so the entity fetch mounts nowhere.
    FlattenWrongKey,
    /// The input fetch stops fetching the `@key`, which every representation carries.
    DropKeyField,
    /// The entity fetch is put under a condition node the operation does not share.
    GuardEntityFetch,
    /// The input is fetched and read under an alias, which an input key renamer names back to the
    /// field the subgraph demands. This is what a planner does when two fetches need one field
    /// with different subselections, and the plan is correct.
    AliasRequiredInput,
    /// The same alias, with no renamer to name it back: the subgraph is sent `__require_0_f`.
    AliasWithoutRenamer,
    /// A renamer naming an alias back that no entry reads under. The plan fetches and reads the
    /// plain field, so the rename has nothing to carry and leaves the entry alone; correct.
    RenamerWithoutAlias,
    /// A renamer naming the demanded field away, so the subgraph is sent a key it never asked for.
    RenameRequiredFieldAway,
    /// Only one entity type aliases its input, under a renamer scoped to nothing. The other type's
    /// entry reads the field under its own name, so the renamer is not one of its rewrites; correct.
    UnscopedRenamerAcrossTypes,
    /// The same, with the renamer scoped to the type that aliases. A renamer testing the type of
    /// what it renames is rejected outright.
    RenamerScopedToItsType,
    /// Each entity type aliases its input differently, under its own renamer scoped to it. Two
    /// renamers onto one key, which is what a planner writes for two types needing the same field;
    /// rejected, the renamers testing types.
    RenamersForDisjointTypes,
    /// The entry reads the demanded field twice, under its alias and under its own name, so two
    /// keys are sent as one and the subgraph gets whichever the rewrites leave.
    OneFieldSentUnderTwoKeys,
    /// The composite half of the demand is read under an alias on the list itself, named back by
    /// a renamer at that key.
    AliasCompositeInput,
    /// The alias is inside the list instead, and the renamer's path reaches it through `@`. The
    /// router renames in every element of the array.
    AliasInsideTheList,
    /// The entity fetch runs in a deferred block that depends on every input fetch.
    DeferOnEveryInput,
    /// The same block naming no dependency, so it waits for the whole primary block.
    DeferOnTheWholePrimary,
    /// The block depends only on the base fetch, which sends it no entity input.
    DeferOnTheBaseFetchOnly,
    /// The block depends only on the first entity type's input fetch. Enough when the case has one
    /// entity type; with two, the second type's representations cannot be built.
    DeferOnTheFirstEntityInput,
    /// Each entry nested in a fragment on the interface, `... on I { ... on T2 { … } }`.
    EntriesNestedUnderTheInterface,
    /// One entry on the interface for every entity type, which also reads `T1` objects.
    OneEntryOnTheInterface,
    /// Each entry under `@include` of the case's guard, or of `v0` when the case has none.
    EntriesIncludedByTheGuard,
    /// Each entry under `@skip` of the same variable.
    EntriesSkippedByTheGuard,
    /// The required fields inside each entry under `@include` of that variable.
    RequiredFieldsUnderACondition,
    /// The required fields inside each entry under `... on I`, which admits the entry's type.
    RequiredFieldsUnderTheInterface,
    /// The required fields inside each entry under `... on T1`, which never matches it.
    RequiredFieldsUnderASiblingType,
    /// The key field inside each entry under `@include` of that variable.
    KeyUnderACondition,
    /// One entity fetch per entity type, in parallel, each reading the same buffer.
    EntityFetchPerType,
    /// The same fetches in sequence, so the second reads what the first wrote.
    EntityFetchesInSequence,
    /// The flatten path conditioned on the entity types the fetch has cases for, `is.@|[T2,T3]`.
    ConditionedFlattenPath,
    /// The path conditioned on the first of them only.
    ConditionMissesAType,
    /// The path conditioned on every implementation of `I`, `T1` included.
    ConditionOnEveryImplementation,
    /// The same, with no fetch selecting `__typename` at `is` outside a fragment, so the router
    /// has no `__typename` to test on a `T1` unless the client asked for it.
    ConditionedPathWithoutTypename,
    /// The entity fetch also selects `cs { a }`, which is `@external` in Subgraph2.
    EntityFetchSelectsExternal,
    /// Under `peer`, the entity fetch also selects `cs { a }`, which `@provides` does not cover.
    ProvidesWindowExceeded,
    /// The interface object's entry is `... on T2` rather than `... on I`, so the router sends
    /// Subgraph3 only `T2` representations and `h` is never fetched for the others.
    InterfaceObjectEntryForOneType,
    /// The interface object's fetch without the value setter naming every representation `I`. The
    /// router would send Subgraph3 `__typename`s it does not know; see the README.
    InterfaceObjectWithoutTypenameSetter,
    /// The value setter names every representation `T2` instead, running another type's resolver.
    InterfaceObjectSentAsAnotherType,
    /// A second value setter overwrites every representation's `id`, sending one key for all.
    InterfaceObjectKeyOverwritten,
}

pub const PERTURBATIONS: [Perturbation; 42] = [
    Perturbation::None,
    Perturbation::DropRequiredField,
    Perturbation::DropRequiresEntry,
    Perturbation::DropEntityCase,
    Perturbation::WrongKeyField,
    Perturbation::FlattenWrongKey,
    Perturbation::DropKeyField,
    Perturbation::GuardEntityFetch,
    Perturbation::AliasRequiredInput,
    Perturbation::AliasWithoutRenamer,
    Perturbation::RenamerWithoutAlias,
    Perturbation::RenameRequiredFieldAway,
    Perturbation::UnscopedRenamerAcrossTypes,
    Perturbation::RenamerScopedToItsType,
    Perturbation::RenamersForDisjointTypes,
    Perturbation::OneFieldSentUnderTwoKeys,
    Perturbation::AliasCompositeInput,
    Perturbation::AliasInsideTheList,
    Perturbation::DeferOnEveryInput,
    Perturbation::DeferOnTheWholePrimary,
    Perturbation::DeferOnTheBaseFetchOnly,
    Perturbation::DeferOnTheFirstEntityInput,
    Perturbation::EntriesNestedUnderTheInterface,
    Perturbation::OneEntryOnTheInterface,
    Perturbation::EntriesIncludedByTheGuard,
    Perturbation::EntriesSkippedByTheGuard,
    Perturbation::RequiredFieldsUnderACondition,
    Perturbation::RequiredFieldsUnderTheInterface,
    Perturbation::RequiredFieldsUnderASiblingType,
    Perturbation::KeyUnderACondition,
    Perturbation::EntityFetchPerType,
    Perturbation::EntityFetchesInSequence,
    Perturbation::ConditionedFlattenPath,
    Perturbation::ConditionMissesAType,
    Perturbation::ConditionOnEveryImplementation,
    Perturbation::ConditionedPathWithoutTypename,
    Perturbation::EntityFetchSelectsExternal,
    Perturbation::ProvidesWindowExceeded,
    Perturbation::InterfaceObjectEntryForOneType,
    Perturbation::InterfaceObjectWithoutTypenameSetter,
    Perturbation::InterfaceObjectSentAsAnotherType,
    Perturbation::InterfaceObjectKeyOverwritten,
];

/// The alias a planner gives a `@requires` input it has to fetch apart from the client's own
/// selection of the same field.
pub const INPUT_ALIAS: &str = "__require_0_f";

/// The composite half of the `@requires` field set: a list of objects, so that a field set can
/// nest and a rewrite path can run through `@`.
pub const COMPOSITE_FIELD: &str = "cs";

/// The one field of the objects in that list.
pub const ELEMENT_FIELD: &str = "a";

/// The aliases the composite half is read under: one on the list itself, one inside its elements.
pub const COMPOSITE_ALIAS: &str = "__require_0_cs";
pub const ELEMENT_ALIAS: &str = "__require_0_a";

/// The alias the second entity type uses where the two differ, so that one key is renamed to from
/// two places.
pub const SECOND_ALIAS: &str = "__require_1_f";

/// The key a renamer names the demanded field away to, which no entry sends.
pub const RENAMED_AWAY: &str = "__sent_nowhere";

/// The entity type that aliases its input where only one of them does.
const ALIASED_TYPE: &str = "T2";

/// The aliases, if any, the composite half of the demand is read under: on the list itself, and
/// on the field inside its elements.
fn composite_aliases(case: &Case) -> (Option<&'static str>, Option<&'static str>) {
    match case.perturbation {
        Perturbation::AliasCompositeInput => (Some(COMPOSITE_ALIAS), None),
        Perturbation::AliasInsideTheList => (None, Some(ELEMENT_ALIAS)),
        _ => (None, None),
    }
}

/// The composite half as the plan fetches it and the entry reads it, in selection syntax.
fn composite_body(case: &Case) -> String {
    let (list_alias, element_alias) = composite_aliases(case);
    let list = list_alias.map_or(String::new(), |alias| format!("{alias}: "));
    let element = element_alias.map_or(String::new(), |alias| format!("{alias}: "));
    format!("{list}{COMPOSITE_FIELD} {{ {element}{ELEMENT_FIELD} }}")
}

/// The alias, if any, that `entity_type`'s `@requires` input is fetched and read under.
fn input_alias(case: &Case, entity_type: &str) -> Option<&'static str> {
    match case.perturbation {
        Perturbation::AliasRequiredInput
        | Perturbation::AliasWithoutRenamer
        | Perturbation::OneFieldSentUnderTwoKeys => Some(INPUT_ALIAS),
        Perturbation::UnscopedRenamerAcrossTypes | Perturbation::RenamerScopedToItsType => {
            (entity_type == ALIASED_TYPE).then_some(INPUT_ALIAS)
        }
        Perturbation::RenamersForDisjointTypes => Some(if entity_type == ALIASED_TYPE {
            INPUT_ALIAS
        } else {
            SECOND_ALIAS
        }),
        _ => None,
    }
}

/// Decodes one case. `None` when the bytes run out, which is how libFuzzer's short inputs are
/// discarded rather than silently padded.
pub fn decode_case(bytes: &[u8]) -> Option<Case> {
    let mut cursor = bytes.iter().copied();
    let count = 1 + (cursor.next()? as usize % MAX_SELECTIONS);
    let mut slots = Vec::new();
    for _ in 0..count {
        let slot = cursor.next()? % SELECTION_SLOTS;
        if !slots.contains(&slot) {
            slots.push(slot);
        }
    }
    let guard_byte = cursor.next()?;
    let guard = match guard_byte % 3 {
        0 => None,
        other => Some(VARIABLES[(other - 1) as usize % VARIABLES.len()]),
    };
    let perturbation = PERTURBATIONS[cursor.next()? as usize % PERTURBATIONS.len()];
    Some(Case {
        slots,
        guard,
        perturbation,
    })
}

impl Case {
    /// The client operation, as SDL.
    pub fn operation_source(&self) -> String {
        let mut body = String::new();
        for slot in &self.slots {
            let guard = match (self.guard, slot_needs_entity_fetch(*slot)) {
                (Some(variable), Some(_)) => format!(" @include(if: ${variable})"),
                _ => String::new(),
            };
            let source = selection_source(*slot);
            // A guard on a fragment goes on the fragment; on a field, on the field.
            let guarded = match source.strip_prefix("... on ") {
                Some(rest) => {
                    let (type_name, inner) = rest.split_once(' ').expect("fragment body");
                    format!("... on {type_name}{guard} {inner}")
                }
                None => format!("{source}{guard}"),
            };
            body.push_str(&guarded);
            body.push(' ');
        }
        // A variable must be declared only when it is used, and it is used only when some
        // selection needs an entity fetch.
        let declarations = match self.guard {
            Some(variable) if self.entity_types().next().is_some() => {
                format!("query(${variable}: Boolean!)")
            }
            _ => "query".to_string(),
        };
        format!("{declarations} {{ is {{ {body}}} }}")
    }

    /// Whether the operation selects the field Subgraph3's interface object resolves.
    pub fn uses_interface_object(&self) -> bool {
        self.slots.contains(&INTERFACE_OBJECT_SLOT)
    }

    /// The entity types this operation needs an entity fetch for, in fixture order.
    pub fn entity_types(&self) -> impl Iterator<Item = &'static str> + '_ {
        ENTITY_TYPES.into_iter().filter(move |entity_type| {
            self.slots
                .iter()
                .any(|slot| slot_needs_entity_fetch(*slot) == Some(*entity_type))
        })
    }
}

//==================================================================================================
// The miniature planner
//==================================================================================================

/// What the base fetch to Subgraph1 selects under `is`: everything the operation asks for that
/// Subgraph1 resolves without an entity fetch.
fn base_fetch_body(case: &Case) -> String {
    // Without `__typename` the body would be empty for a case selecting only entity fields.
    let mut body = String::from(
        if case.perturbation == Perturbation::ConditionedPathWithoutTypename {
            "id "
        } else {
            "__typename "
        },
    );
    for slot in &case.slots {
        if let Some(fetched) = fetched_source(*slot) {
            body.push_str(&fetched);
            body.push(' ');
        }
    }
    // The interface object's key, which its representations carry.
    if case.uses_interface_object() && case.perturbation != Perturbation::DropKeyField {
        body.push_str(KEY_FIELDS);
        body.push(' ');
    }
    body
}

/// What Subgraph1 is asked for to make one entity type fetchable: `__typename` and the `@key`, and
/// the `@requires` field set of the field Subgraph2 resolves there.
///
/// This is a fetch of its own rather than a branch of the base fetch because `T2.f` is `Int!` and
/// `T3.f` is `Int`: one operation cannot select both under one response name, which is why the
/// real planner aliases one of them. Splitting the fetches says the same thing without the alias,
/// and puts a parallel node in every plan that needs two entity types.
fn entity_input_body(case: &Case, entity_type: &str) -> String {
    let mut inner = String::from("__typename");
    if case.perturbation != Perturbation::DropKeyField {
        inner.push(' ');
        inner.push_str(KEY_FIELDS);
    }
    if case.perturbation != Perturbation::DropRequiredField {
        inner.push(' ');
        match input_alias(case, entity_type) {
            Some(alias) => inner.push_str(&format!("{alias}: {REQUIRES_FIELDS}")),
            None => inner.push_str(REQUIRES_FIELDS),
        }
        // The entry reads the field under both keys, so the plan has to have fetched both.
        if case.perturbation == Perturbation::OneFieldSentUnderTwoKeys {
            inner.push(' ');
            inner.push_str(REQUIRES_FIELDS);
        }
        inner.push(' ');
        inner.push_str(&composite_body(case));
    }
    if case.perturbation == Perturbation::ConditionedPathWithoutTypename {
        format!("... on {entity_type} {{ {inner} }} ")
    } else {
        format!("__typename ... on {entity_type} {{ {inner} }} ")
    }
}

/// The variable a conditioned entry tests: the case's guard, or `v0` when it has none.
fn entry_variable(case: &Case) -> &'static str {
    case.guard.unwrap_or(VARIABLES[0])
}

fn entry_condition(
    kind: requires_selection::ConditionKind,
    variable: &str,
) -> requires_selection::Condition {
    requires_selection::Condition {
        kind,
        if_: requires_selection::ConditionValue::Variable(Name::new(variable).expect("variable")),
    }
}

fn entry_fragment(
    type_condition: Option<&str>,
    conditions: Vec<requires_selection::Condition>,
    selections: Vec<requires_selection::Selection>,
) -> requires_selection::Selection {
    requires_selection::Selection::InlineFragment(requires_selection::InlineFragment {
        type_condition: type_condition.map(|name| Name::new(name).expect("type name")),
        conditions,
        selections,
    })
}

fn entry_field(
    alias: Option<&str>,
    name: &str,
    selections: Vec<requires_selection::Selection>,
) -> requires_selection::Selection {
    requires_selection::Selection::Field(requires_selection::Field {
        alias: alias.map(|alias| Name::new(alias).expect("alias")),
        name: Name::new(name).expect("field name"),
        selections,
    })
}

/// What one entity type's entry reads inside its fragment.
fn entry_body(case: &Case, entity_type: &str) -> Vec<requires_selection::Selection> {
    let key_field = if case.perturbation == Perturbation::WrongKeyField {
        REQUIRES_FIELDS
    } else {
        KEY_FIELDS
    };
    let include = || {
        vec![entry_condition(
            requires_selection::ConditionKind::Include,
            entry_variable(case),
        )]
    };
    let mut key = vec![entry_field(None, key_field, Vec::new())];
    if case.perturbation == Perturbation::KeyUnderACondition {
        key = vec![entry_fragment(None, include(), key)];
    }
    // The entry reads a field under its alias, which is where the plan fetched it; the renamer
    // names it back to what the subgraph demands.
    let mut required = vec![entry_field(
        input_alias(case, entity_type),
        REQUIRES_FIELDS,
        Vec::new(),
    )];
    if case.perturbation == Perturbation::OneFieldSentUnderTwoKeys {
        required.push(entry_field(None, REQUIRES_FIELDS, Vec::new()));
    }
    let (list_alias, element_alias) = composite_aliases(case);
    required.push(entry_field(
        list_alias,
        COMPOSITE_FIELD,
        vec![entry_field(element_alias, ELEMENT_FIELD, Vec::new())],
    ));
    let required = match case.perturbation {
        Perturbation::RequiredFieldsUnderACondition => {
            vec![entry_fragment(None, include(), required)]
        }
        Perturbation::RequiredFieldsUnderTheInterface => {
            vec![entry_fragment(Some("I"), Vec::new(), required)]
        }
        Perturbation::RequiredFieldsUnderASiblingType => {
            vec![entry_fragment(Some("T1"), Vec::new(), required)]
        }
        _ => required,
    };
    let mut body = vec![entry_field(None, "__typename", Vec::new())];
    body.extend(key);
    body.extend(required);
    body
}

/// The `requires` entries of an entity fetch with cases for `entity_types`: one per entity case,
/// naming what the plan claims to have already fetched about it.
fn requires_entries(case: &Case, entity_types: &[&str]) -> Vec<requires_selection::Selection> {
    if case.perturbation == Perturbation::OneEntryOnTheInterface {
        let Some(first) = entity_types.first() else {
            return Vec::new();
        };
        return vec![entry_fragment(
            Some("I"),
            Vec::new(),
            entry_body(case, first),
        )];
    }
    let mut entries: Vec<requires_selection::Selection> = entity_types
        .iter()
        .map(|entity_type| {
            let body = entry_body(case, entity_type);
            match case.perturbation {
                Perturbation::EntriesNestedUnderTheInterface => entry_fragment(
                    Some("I"),
                    Vec::new(),
                    vec![entry_fragment(Some(entity_type), Vec::new(), body)],
                ),
                Perturbation::EntriesIncludedByTheGuard => entry_fragment(
                    Some(entity_type),
                    vec![entry_condition(
                        requires_selection::ConditionKind::Include,
                        entry_variable(case),
                    )],
                    body,
                ),
                Perturbation::EntriesSkippedByTheGuard => entry_fragment(
                    Some(entity_type),
                    vec![entry_condition(
                        requires_selection::ConditionKind::Skip,
                        entry_variable(case),
                    )],
                    body,
                ),
                _ => entry_fragment(Some(entity_type), Vec::new(), body),
            }
        })
        .collect();
    if case.perturbation == Perturbation::DropRequiresEntry && entries.len() > 1 {
        entries.pop();
    }
    entries
}

/// The entity cases of the entity fetch: what it asks of each entity type it is given.
fn entity_cases(case: &Case) -> Vec<&'static str> {
    let mut cases: Vec<&'static str> = case.entity_types().collect();
    if case.perturbation == Perturbation::DropEntityCase && cases.len() > 1 {
        cases.pop();
    }
    cases
}

/// The path the entity fetch is flattened to.
///
/// Dropping the index would *not* be a perturbation: an index consumes no response key, so `is`
/// and `is.@` mount to the same place. Naming a key the plan never fetched does mount nowhere.
///
/// A conditioned path names the entity types `cases` the fetch has cases for, only the first, or
/// every implementation of `I`.
fn flatten_path(case: &Case, cases: &[&str]) -> Vec<FetchDataPathElement> {
    let key = if case.perturbation == Perturbation::FlattenWrongKey {
        "g"
    } else {
        "is"
    };
    let names = |types: &[&str]| {
        Some(
            types
                .iter()
                .map(|name| Name::new(name).expect("type name"))
                .collect(),
        )
    };
    let condition = match case.perturbation {
        Perturbation::ConditionedFlattenPath => names(cases),
        Perturbation::ConditionOnEveryImplementation
        | Perturbation::ConditionedPathWithoutTypename => names(&OBJECT_TYPES),
        Perturbation::ConditionMissesAType => names(&cases[..1.min(cases.len())]),
        _ => None,
    };
    vec![
        FetchDataPathElement::Key(Name::new(key).expect("field name"), Default::default()),
        FetchDataPathElement::AnyIndex(condition),
    ]
}

/// What the entity fetch selects in `entity_type`'s case: `g` and `peer { f }` as the operation
/// asks for them.
fn entity_selections(case: &Case, entity_type: &str) -> String {
    let asks =
        |slot: u8| case.slots.contains(&slot) && slot_needs_entity_fetch(slot) == Some(entity_type);
    let mut selected = String::new();
    if asks(4) || asks(5) {
        selected.push_str("g ");
    }
    if asks(8) {
        if case.perturbation == Perturbation::ProvidesWindowExceeded {
            selected.push_str("peer { f cs { a } } ");
        } else {
            selected.push_str("peer { f } ");
        }
    }
    if case.perturbation == Perturbation::EntityFetchSelectsExternal {
        selected.push_str("cs { a } ");
    }
    selected
}

/// The fetch asking Subgraph3's interface object for `h`: one entry at `I`, and the value setter
/// naming every representation `I`, as the planner writes them.
fn interface_object_fetch(
    subgraphs: &apollo_compiler::collections::IndexMap<Arc<str>, ValidFederationSchema>,
    case: &Case,
) -> PlanNode {
    let entry_type = if case.perturbation == Perturbation::InterfaceObjectEntryForOneType {
        "T2"
    } else {
        "I"
    };
    let requires = vec![entry_fragment(
        Some(entry_type),
        Vec::new(),
        vec![
            entry_field(None, "__typename", Vec::new()),
            entry_field(None, KEY_FIELDS, Vec::new()),
        ],
    )];
    let setter = |key: &str, value: &str| {
        Arc::new(FetchDataRewrite::ValueSetter(FetchDataValueSetter {
            path: vec![
                FetchDataPathElement::TypenameEquals(Name::new("I").expect("type name")),
                FetchDataPathElement::Key(Name::new(key).expect("key"), None),
            ],
            set_value_to: value.into(),
        }))
    };
    let input_rewrites = match case.perturbation {
        Perturbation::InterfaceObjectWithoutTypenameSetter => Vec::new(),
        Perturbation::InterfaceObjectSentAsAnotherType => vec![setter("__typename", "T2")],
        Perturbation::InterfaceObjectKeyOverwritten => {
            vec![setter("__typename", "I"), setter(KEY_FIELDS, "1")]
        }
        _ => vec![setter("__typename", "I")],
    };
    PlanNode::Flatten(FlattenNode {
        path: vec![
            FetchDataPathElement::Key(Name::new("is").expect("field name"), None),
            FetchDataPathElement::AnyIndex(None),
        ],
        node: Box::new(PlanNode::Fetch(Box::new(fetch_node(
            subgraphs.get("Subgraph3").expect("Subgraph3"),
            "Subgraph3",
            None,
            "query($representations: [_Any!]!) { _entities(representations: $representations) { ... on I { h } } }",
            requires,
            input_rewrites,
        )))),
    })
}

/// The base fetch's id. The entity input fetches follow it, one per entity type in case order.
const BASE_FETCH_ID: u64 = 0;

/// The ids a deferred block depends on, when the case puts the entity fetch in one; `None` when it
/// does not.
fn deferred_dependencies(case: &Case) -> Option<Vec<u64>> {
    let entity_inputs = case.entity_types().count() as u64;
    match case.perturbation {
        Perturbation::DeferOnEveryInput => Some((BASE_FETCH_ID..=entity_inputs).collect()),
        Perturbation::DeferOnTheWholePrimary => Some(Vec::new()),
        Perturbation::DeferOnTheBaseFetchOnly => Some(vec![BASE_FETCH_ID]),
        Perturbation::DeferOnTheFirstEntityInput => Some(vec![BASE_FETCH_ID + 1]),
        _ => None,
    }
}

/// The entity fetch's input rewrites: the key renamers applied to each representation before it is
/// sent. Only an entity fetch has them, a root fetch sending no representation.
fn input_rewrites(case: &Case) -> Vec<Arc<FetchDataRewrite>> {
    // `scope` is the `typenameEquals` filter that restricts a renamer to one runtime type, which
    // is how a planner keeps one type's alias from reaching another type's representation.
    let renamer = |scope: Option<&str>, from: &str, to: &str| {
        let mut path = Vec::new();
        if let Some(type_name) = scope {
            path.push(FetchDataPathElement::TypenameEquals(
                Name::new(type_name).expect("type name"),
            ));
        }
        path.push(FetchDataPathElement::Key(
            Name::new(from).expect("key"),
            None,
        ));
        Arc::new(FetchDataRewrite::KeyRenamer(FetchDataKeyRenamer {
            path,
            rename_key_to: Name::new(to).expect("key"),
        }))
    };
    match case.perturbation {
        Perturbation::AliasRequiredInput
        | Perturbation::RenamerWithoutAlias
        | Perturbation::UnscopedRenamerAcrossTypes
        | Perturbation::OneFieldSentUnderTwoKeys => {
            vec![renamer(None, INPUT_ALIAS, REQUIRES_FIELDS)]
        }
        Perturbation::RenamerScopedToItsType => {
            vec![renamer(Some(ALIASED_TYPE), INPUT_ALIAS, REQUIRES_FIELDS)]
        }
        Perturbation::RenamersForDisjointTypes => ENTITY_TYPES
            .iter()
            .map(|entity_type| {
                let alias = if *entity_type == ALIASED_TYPE {
                    INPUT_ALIAS
                } else {
                    SECOND_ALIAS
                };
                renamer(Some(entity_type), alias, REQUIRES_FIELDS)
            })
            .collect(),
        Perturbation::RenameRequiredFieldAway => {
            vec![renamer(None, REQUIRES_FIELDS, RENAMED_AWAY)]
        }
        Perturbation::AliasCompositeInput => {
            vec![renamer(None, COMPOSITE_ALIAS, COMPOSITE_FIELD)]
        }
        // Through `@`: the alias is on a field inside each element of the list.
        Perturbation::AliasInsideTheList => {
            vec![Arc::new(FetchDataRewrite::KeyRenamer(
                FetchDataKeyRenamer {
                    path: vec![
                        FetchDataPathElement::Key(Name::new(COMPOSITE_FIELD).expect("key"), None),
                        FetchDataPathElement::AnyIndex(None),
                        FetchDataPathElement::Key(Name::new(ELEMENT_ALIAS).expect("key"), None),
                    ],
                    rename_key_to: Name::new(ELEMENT_FIELD).expect("key"),
                },
            ))]
        }
        _ => Vec::new(),
    }
}

fn fetch_node(
    schema: &ValidFederationSchema,
    subgraph_name: &str,
    id: Option<u64>,
    operation: &str,
    requires: Vec<requires_selection::Selection>,
    input_rewrites: Vec<Arc<FetchDataRewrite>>,
) -> FetchNode {
    let document =
        ExecutableDocument::parse_and_validate(schema.schema(), operation, "fetch.graphql")
            .unwrap_or_else(|error| panic!("fetch operation must be valid:\n{operation}\n{error}"));
    FetchNode {
        subgraph_name: subgraph_name.into(),
        protocol: Default::default(),
        id,
        variable_usages: Vec::new(),
        requires,
        operation_document: SerializableDocument::from_parsed(Arc::new(document)),
        operation_name: None,
        operation_kind: apollo_compiler::executable::OperationType::Query,
        input_rewrites: Arc::new(input_rewrites),
        output_rewrites: Vec::new(),
        context_rewrites: Vec::new(),
    }
}

/// The plan the miniature planner gives one case, with its perturbation applied.
///
/// The subgraph operations are validated against their own subgraph schemas, so a grammar change
/// that makes them invalid fails loudly here rather than turning into a checker verdict.
pub fn build_plan(
    subgraphs: &apollo_compiler::collections::IndexMap<Arc<str>, ValidFederationSchema>,
    case: &Case,
) -> QueryPlan {
    let subgraph1 = subgraphs.get("Subgraph1").expect("Subgraph1");
    let cases = entity_cases(case);
    let mut inputs = vec![PlanNode::Fetch(Box::new(fetch_node(
        subgraph1,
        "Subgraph1",
        Some(BASE_FETCH_ID),
        &format!("{{ is {{ {} }} }}", base_fetch_body(case)),
        Vec::new(),
        Vec::new(),
    )))];
    // One input fetch per entity type the case needs, whether or not the entity fetch keeps a
    // case for it: `DropEntityCase` drops a case from the entity fetch, not the input.
    for (entity_type, id) in case.entity_types().zip(BASE_FETCH_ID + 1..) {
        inputs.push(PlanNode::Fetch(Box::new(fetch_node(
            subgraph1,
            "Subgraph1",
            Some(id),
            &format!("{{ is {{ {} }} }}", entity_input_body(case, entity_type)),
            Vec::new(),
            Vec::new(),
        ))));
    }

    // The interface object's fetch reads only the base fetch, so it runs right after the inputs,
    // outside any condition or deferred block the entity fetch is put in.
    let inputs = if case.uses_interface_object() {
        vec![PlanNode::Sequence(SequenceNode {
            nodes: vec![
                PlanNode::Parallel(ParallelNode { nodes: inputs }),
                interface_object_fetch(subgraphs, case),
            ],
        })]
    } else {
        inputs
    };
    if cases.is_empty() {
        return QueryPlan {
            node: Some(TopLevelPlanNode::Parallel(ParallelNode { nodes: inputs })),
            statistics: Default::default(),
        };
    }

    let subgraph2 = subgraphs.get("Subgraph2").expect("Subgraph2");
    let entity_fetch = |cases: &[&str]| {
        let body: String = cases
            .iter()
            .map(|entity_type| {
                format!(
                    "... on {entity_type} {{ {}}} ",
                    entity_selections(case, entity_type)
                )
            })
            .collect();
        PlanNode::Flatten(FlattenNode {
            path: flatten_path(case, cases),
            node: Box::new(PlanNode::Fetch(Box::new(fetch_node(
                subgraph2,
                "Subgraph2",
                None,
                &format!("query($representations: [_Any!]!) {{ _entities(representations: $representations) {{ {body}}} }}"),
                requires_entries(case, cases),
                input_rewrites(case),
            )))),
        })
    };
    let split: Vec<PlanNode> = cases.iter().map(|one| entity_fetch(&[one])).collect();
    let mut mounted = match case.perturbation {
        Perturbation::EntityFetchPerType if split.len() > 1 => {
            PlanNode::Parallel(ParallelNode { nodes: split })
        }
        Perturbation::EntityFetchesInSequence if split.len() > 1 => {
            PlanNode::Sequence(SequenceNode { nodes: split })
        }
        _ => entity_fetch(&cases),
    };
    // The operation's own guard is compiled into a condition node above the fetch it guards, which
    // is what makes a guarded case correct; `GuardEntityFetch` adds one the operation lacks.
    let condition = match (case.guard, case.perturbation) {
        (_, Perturbation::GuardEntityFetch) => Some(VARIABLES[VARIABLES.len() - 1]),
        (Some(variable), _) => Some(variable),
        (None, _) => None,
    };
    if let Some(variable) = condition {
        mounted = PlanNode::Condition(Box::new(apollo_federation::query_plan::ConditionNode {
            condition_variable: Name::new(variable).expect("variable name"),
            if_clause: Some(Box::new(mounted)),
            else_clause: None,
        }));
    }

    if let Some(depends) = deferred_dependencies(case) {
        // The inputs are the primary block and the entity fetch is deferred: it reads what its
        // dependencies send, or the whole primary block when it names none.
        return QueryPlan {
            node: Some(TopLevelPlanNode::Defer(DeferNode {
                primary: PrimaryDeferBlock {
                    sub_selection: None,
                    node: Some(Box::new(PlanNode::Parallel(ParallelNode { nodes: inputs }))),
                },
                deferred: vec![DeferredDeferBlock {
                    depends: depends
                        .into_iter()
                        .map(|id| DeferredDependency { id: id.to_string() })
                        .collect(),
                    label: None,
                    query_path: Vec::new(),
                    sub_selection: None,
                    node: Some(Box::new(mounted)),
                }],
            })),
            statistics: Default::default(),
        };
    }

    QueryPlan {
        node: Some(TopLevelPlanNode::Sequence(SequenceNode {
            nodes: vec![PlanNode::Parallel(ParallelNode { nodes: inputs }), mounted],
        })),
        statistics: Default::default(),
    }
}

/// The client operation of a case, validated against the API schema.
pub fn build_operation(
    api_schema: &ValidFederationSchema,
    case: &Case,
) -> Option<Valid<ExecutableDocument>> {
    ExecutableDocument::parse_and_validate(
        api_schema.schema(),
        case.operation_source(),
        "operation.graphql",
    )
    .ok()
}

/// Must this case be accepted?
///
/// Only the unperturbed plan, which the miniature planner built for this very operation. Whether a
/// *perturbed* plan is wrong is not something this grammar can decide: `DropKeyField` breaks
/// nothing when the operation selects `id` itself, and working that out is the checker's job, not
/// the generator's. The Lean lane is the oracle for those; here they are coverage.
pub fn must_be_accepted(case: &Case) -> bool {
    case.perturbation == Perturbation::None
}

/// Every byte string the grammar reads, with exactly the bytes each decision consumes.
///
/// The slot count decides how many slot bytes follow, so a fixed-width enumeration would feed the
/// guard byte to the perturbation and leave the grammar unevenly covered. For the same reason a
/// byte string written by hand is rarely the case its author meant.
pub fn enumerate_inputs() -> Vec<Vec<u8>> {
    let mut inputs = Vec::new();
    for count in 0..MAX_SELECTIONS as u8 {
        let selections = count as usize + 1;
        let mut slots = vec![0u8; selections];
        'odometer: loop {
            for guard in 0..3u8 {
                for perturbation in 0..PERTURBATIONS.len() as u8 {
                    let mut input = vec![count];
                    input.extend(slots.iter().copied());
                    input.push(guard);
                    input.push(perturbation);
                    inputs.push(input);
                }
            }
            let mut position = 0;
            loop {
                if position == selections {
                    break 'odometer;
                }
                slots[position] += 1;
                if slots[position] < SELECTION_SLOTS {
                    break;
                }
                slots[position] = 0;
                position += 1;
            }
        }
    }
    inputs
}
