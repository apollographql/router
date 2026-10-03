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
use apollo_federation::query_plan::FetchDataPathElement;
use apollo_federation::query_plan::FetchNode;
use apollo_federation::query_plan::FlattenNode;
use apollo_federation::query_plan::ParallelNode;
use apollo_federation::query_plan::PlanNode;
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
pub const DIGEST_TYPES: [&str; 5] = ["Query", "I", "T1", "T2", "T3"];

/// A canonical rendering of everything the two sides must agree on, so the oracle can prove it
/// holds the same fixture. Compared before any case runs.
///
/// The field declarations are read from the real supergraph schema rather than from constants
/// here, so that a drift between the checked-in SDL and the oracle's hand-written tables is
/// caught. `__typename` is listed explicitly: the Lean side has to declare it, its `Schema` not
/// modelling introspection, and apollo-compiler knows it natively — so it is part of what the two
/// must agree on even though only one of them writes it down. Wrappers are dropped, neither
/// checker looking past a field's named type.
pub fn schema_digest(schema: &ValidFederationSchema) -> String {
    let objects = OBJECT_TYPES.join(" ");
    let entities = ENTITY_TYPES.join(" ");
    let variables = VARIABLES.join(" ");
    let selections: Vec<String> = (0..SELECTION_SLOTS).map(selection_source).collect();
    let fields: Vec<String> = DIGEST_TYPES
        .iter()
        .map(|type_name| render_type_fields(schema, type_name))
        .collect();
    format!(
        "objects: {objects} | entities: {entities} | key: {KEY_FIELDS} | \
         requires: {REQUIRES_FIELDS} | variables: {variables} | selections: {} | fields: {}",
        selections.join(" "),
        fields.join(" ")
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
pub const SELECTION_SLOTS: u8 = 8;

/// The most selections one operation carries. Beyond three the plans stop differing in kind.
pub const MAX_SELECTIONS: usize = 3;

/// One selection under `is`, by slot.
///
/// `g` never appears unguarded by a type condition: on `T1` it is resolved by Subgraph1 and on
/// `T2`/`T3` by Subgraph2, so an unguarded `g` would make the planner type-explode and the
/// miniature planner below would stop resembling it.
fn selection_source(slot: u8) -> String {
    match slot % SELECTION_SLOTS {
        0 => "id".to_string(),
        1 => "f".to_string(),
        2 => "__typename".to_string(),
        3 => "... on T1 { g }".to_string(),
        4 => "... on T2 { g }".to_string(),
        5 => "... on T3 { g }".to_string(),
        6 => "... on T2 { f }".to_string(),
        _ => "... on T3 { id }".to_string(),
    }
}

/// Which entity type a slot needs an entity fetch for, if any.
fn slot_needs_entity_fetch(slot: u8) -> Option<&'static str> {
    match slot % SELECTION_SLOTS {
        4 => Some("T2"),
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
}

pub const PERTURBATIONS: [Perturbation; 8] = [
    Perturbation::None,
    Perturbation::DropRequiredField,
    Perturbation::DropRequiresEntry,
    Perturbation::DropEntityCase,
    Perturbation::WrongKeyField,
    Perturbation::FlattenWrongKey,
    Perturbation::DropKeyField,
    Perturbation::GuardEntityFetch,
];

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
    let mut body = String::from("__typename ");
    for slot in &case.slots {
        if slot_needs_entity_fetch(*slot).is_some() {
            continue;
        }
        body.push_str(&selection_source(*slot));
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
        inner.push_str(REQUIRES_FIELDS);
    }
    format!("__typename ... on {entity_type} {{ {inner} }} ")
}

/// The `requires` entries of the entity fetch: one per entity case, naming what the plan claims to
/// have already fetched about it.
fn requires_entries(case: &Case) -> Vec<requires_selection::Selection> {
    let key_field = if case.perturbation == Perturbation::WrongKeyField {
        REQUIRES_FIELDS
    } else {
        KEY_FIELDS
    };
    let mut entries: Vec<requires_selection::Selection> = case
        .entity_types()
        .map(|entity_type| {
            requires_selection::Selection::InlineFragment(requires_selection::InlineFragment {
                type_condition: Some(Name::new(entity_type).expect("type name")),
                selections: ["__typename", key_field, REQUIRES_FIELDS]
                    .into_iter()
                    .map(|name| {
                        requires_selection::Selection::Field(requires_selection::Field {
                            alias: None,
                            name: Name::new(name).expect("field name"),
                            selections: Vec::new(),
                        })
                    })
                    .collect(),
            })
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
fn flatten_path(case: &Case) -> Vec<FetchDataPathElement> {
    let key = if case.perturbation == Perturbation::FlattenWrongKey {
        "g"
    } else {
        "is"
    };
    vec![
        FetchDataPathElement::Key(Name::new(key).expect("field name"), Default::default()),
        FetchDataPathElement::AnyIndex(Default::default()),
    ]
}

fn fetch_node(
    schema: &ValidFederationSchema,
    subgraph_name: &str,
    operation: &str,
    requires: Vec<requires_selection::Selection>,
) -> FetchNode {
    let document =
        ExecutableDocument::parse_and_validate(schema.schema(), operation, "fetch.graphql")
            .unwrap_or_else(|error| panic!("fetch operation must be valid:\n{operation}\n{error}"));
    FetchNode {
        subgraph_name: subgraph_name.into(),
        id: None,
        variable_usages: Vec::new(),
        requires,
        operation_document: SerializableDocument::from_parsed(Arc::new(document)),
        operation_name: None,
        operation_kind: apollo_compiler::executable::OperationType::Query,
        input_rewrites: Default::default(),
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
        &format!("{{ is {{ {} }} }}", base_fetch_body(case)),
        Vec::new(),
    )))];
    for entity_type in &cases {
        inputs.push(PlanNode::Fetch(Box::new(fetch_node(
            subgraph1,
            "Subgraph1",
            &format!("{{ is {{ {} }} }}", entity_input_body(case, entity_type)),
            Vec::new(),
        ))));
    }

    if cases.is_empty() {
        return QueryPlan {
            node: Some(TopLevelPlanNode::Parallel(ParallelNode { nodes: inputs })),
            statistics: Default::default(),
        };
    }

    let subgraph2 = subgraphs.get("Subgraph2").expect("Subgraph2");
    let body: String = cases
        .iter()
        .map(|entity_type| format!("... on {entity_type} {{ g }} "))
        .collect();
    let entity_fetch = fetch_node(
        subgraph2,
        "Subgraph2",
        &format!("query($representations: [_Any!]!) {{ _entities(representations: $representations) {{ {body}}} }}"),
        requires_entries(case),
    );

    let mut mounted = PlanNode::Flatten(FlattenNode {
        path: flatten_path(case),
        node: Box::new(PlanNode::Fetch(Box::new(entity_fetch))),
    });
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
