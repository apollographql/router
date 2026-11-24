//! The aliased operation a flatten's members share when they go to their subgraph together.
//!
//! Each member's piece of the operation is prepared once per plan, with its names changed so no
//! two pieces collide. Member `i`'s
//!
//! - `_entities` field is aliased `_entities{i}` and reads `$representations{i}`,
//! - other root fields are aliased `_r{i}_{field}`,
//! - fragments are renamed `{fragment}__{i}`.
//!
//! A request joins the pieces of the members it sends, so nothing is parsed per request. The
//! pieces live beside the plan's nodes, in [`BatchOperations`], so the plan itself only says
//! which fetches go out together.

use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::executable;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use indexmap::IndexMap;
use indexmap::IndexSet;

use super::PlanNode;
use super::fetch::FetchNode;
use super::plan::FlattenMember;
use super::plan::FlattenNode;

/// A name in a fetch's own operation, and the name it has in the shared operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Renamed {
    pub(crate) own: Arc<str>,
    pub(crate) shared: Arc<str>,
}

/// The operation a flatten's members share: each member's piece of it, by member.
#[derive(Debug, PartialEq)]
pub(crate) struct BatchOperation {
    pub(crate) pieces: Vec<Arc<MemberPiece>>,
}

/// One member's piece of the operation its flatten shares.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MemberPiece {
    /// The operation type and name, as the fetch's own operation has them.
    head: String,
    /// The fetch's operation directives.
    directives: String,
    /// The keys at the top of the fetch's response.
    pub(crate) keys: Vec<Renamed>,
    /// The fetch's variables, and their definitions in the shared operation.
    pub(crate) variables: Vec<Renamed>,
    definitions: Vec<String>,
    selections: String,
    fragments: Vec<String>,
}

/// The operations of a plan's flattens whose members go out together, by what the members are.
#[allow(dead_code, reason = "sent once flatten members execute")]
#[derive(Debug, Default)]
pub(crate) struct BatchOperations(HashMap<FlattenKey, Arc<BatchOperation>>);

/// What a flatten's shared operation is made from: each member's subgraph, operation, kind and
/// the client variables it reads, in order. Where a member merges does not change it, so its
/// paths are left out.
#[derive(Debug, PartialEq, Eq, Hash)]
struct FlattenKey(Vec<MemberKey>);

#[derive(Debug, PartialEq, Eq, Hash)]
struct MemberKey {
    service_name: Arc<str>,
    operation: String,
    is_entities: bool,
    variable_usages: Vec<Arc<str>>,
}

impl FlattenKey {
    fn of(flatten: &FlattenNode) -> Self {
        let members = flatten.members.iter().map(|member| {
            let fetch = &member.fetch;
            MemberKey {
                service_name: fetch.service_name.clone(),
                operation: fetch.operation.as_serialized().to_string(),
                is_entities: fetch.kind.is_entities(),
                variable_usages: fetch.variable_usages.clone(),
            }
        });
        Self(members.collect())
    }
}

impl BatchOperations {
    /// Prepares the operation of each flatten under `root`.
    pub(crate) fn prepare(root: Option<&PlanNode>) -> Self {
        let mut operations = HashMap::new();
        if let Some(root) = root {
            collect(root, &mut operations);
        }
        Self(operations)
    }

    /// The operation `flatten`'s members share, where `flatten` is a node of the plan these were
    /// prepared from or one with the same members.
    #[allow(dead_code, reason = "sent once flatten members execute")]
    pub(crate) fn get(&self, flatten: &FlattenNode) -> Option<&Arc<BatchOperation>> {
        self.0.get(&FlattenKey::of(flatten))
    }
}

/// Prepares the operation of each flatten under `node` whose members go out together.
fn collect(node: &PlanNode, operations: &mut HashMap<FlattenKey, Arc<BatchOperation>>) {
    match node {
        PlanNode::Sequence { nodes } | PlanNode::Parallel { nodes } => {
            nodes.iter().for_each(|node| collect(node, operations));
        }
        PlanNode::Fetch(_) => {}
        PlanNode::Flatten(flatten) => {
            if flatten.members.len() > 1
                && let Some(operation) = BatchOperation::new(&flatten.members)
            {
                operations.insert(FlattenKey::of(flatten), Arc::new(operation));
            }
        }
        PlanNode::Defer { primary, deferred } => {
            let deferred = deferred
                .iter()
                .filter_map(|deferred| deferred.node.as_deref());
            for node in primary.node.as_deref().into_iter().chain(deferred) {
                collect(node, operations);
            }
        }
        PlanNode::Subscription { rest, .. } => {
            if let Some(rest) = rest.as_deref() {
                collect(rest, operations);
            }
        }
        PlanNode::Condition {
            if_clause,
            else_clause,
            ..
        } => {
            for node in [if_clause, else_clause].into_iter().flatten() {
                collect(node, operations);
            }
        }
    }
}

impl BatchOperation {
    /// Each member's piece of an operation shared by all of them. The planner only puts fetches
    /// to one subgraph with one operation type in a flatten together. None when a member's
    /// operation was not parsed.
    pub(crate) fn new(members: &[FlattenMember]) -> Option<Self> {
        let prefix = renamed_variable_prefix(members)?;
        let pieces = members
            .iter()
            .enumerate()
            .map(|(index, member)| MemberPiece::new(index, &member.fetch, &prefix).map(Arc::new))
            .collect::<Option<_>>()?;
        Some(Self { pieces })
    }
}

/// The operation that sends the members with `pieces` together, and the variables it defines.
/// Its type, name and directives are the first piece's.
#[allow(dead_code, reason = "sent once flatten members execute")]
pub(crate) fn join<'a>(
    pieces: impl IntoIterator<Item = &'a MemberPiece>,
) -> (String, Vec<&'a str>) {
    let fetches: Vec<&MemberPiece> = pieces.into_iter().collect();
    // Members pass a client's variables through under the same name and definition, so each is
    // defined once.
    let mut definitions: IndexMap<&str, &str> = IndexMap::default();
    for fetch in &fetches {
        for (variable, definition) in fetch.variables.iter().zip(&fetch.definitions) {
            definitions.entry(&variable.shared).or_insert(definition);
        }
    }

    let mut operation = String::new();
    if let Some(first) = fetches.first() {
        operation.push_str(&first.head);
    }
    if !definitions.is_empty() {
        let definitions: Vec<&str> = definitions.values().copied().collect();
        operation.push_str(&format!("({})", definitions.join(", ")));
    }
    if let Some(first) = fetches.first().filter(|first| !first.directives.is_empty()) {
        operation.push(' ');
        operation.push_str(&first.directives);
    }
    operation.push_str(" {");
    for fetch in &fetches {
        operation.push(' ');
        operation.push_str(&fetch.selections);
    }
    operation.push_str(" }");
    for fragment in fetches.iter().flat_map(|fetch| &fetch.fragments) {
        operation.push(' ');
        operation.push_str(fragment);
    }
    (operation, definitions.into_keys().collect())
}

impl MemberPiece {
    /// Member `index`'s part, from its fetch's parsed operation. Renamed variables start with
    /// `prefix`.
    fn new(index: usize, fetch: &FetchNode, prefix: &str) -> Option<Self> {
        let document = fetch.operation.as_parsed().ok()?;
        let operation = document.operations.iter().next()?;
        let is_entities = fetch.kind.is_entities();
        let renames = Renames::new(index, operation, document, is_entities, prefix);

        let mut selection_set = operation.selection_set.clone();
        inline_top_level_spreads(&mut selection_set, document)?;
        let keys = alias_top_level_fields(&mut selection_set, |field| {
            if is_entities {
                Name::new(&format!("{field}{index}")).ok()
            } else {
                Name::new(&format!("_r{index}_{field}")).ok()
            }
        })?;

        let mut fragments = Vec::new();
        for name in spread_fragments(&selection_set, document) {
            let mut fragment = document.fragments.get(&name)?.clone();
            let fragment_mut = fragment.make_mut();
            fragment_mut.name = renames.fragments[&fragment_mut.name].clone();
            renames.selection_set(&mut fragment_mut.selection_set);
            fragments.push(fragment.serialize().no_indent().to_string());
        }
        renames.selection_set(&mut selection_set);

        let mut variables = Vec::with_capacity(operation.variables.len());
        let mut definitions = Vec::with_capacity(operation.variables.len());
        for definition in &operation.variables {
            let shared = renames.variable(&definition.name);
            let mut renamed = definition.clone();
            renamed.make_mut().name = shared.clone();
            definitions.push(renamed.serialize().no_indent().to_string());
            variables.push(Renamed {
                own: definition.name.as_str().into(),
                shared: shared.as_str().into(),
            });
        }

        Some(Self {
            head: match &operation.name {
                Some(name) => format!("{} {name}", operation.operation_type),
                None => operation.operation_type.to_string(),
            },
            directives: operation.directives.serialize().no_indent().to_string(),
            keys,
            variables,
            definitions,
            selections: serialize_selections(&selection_set),
            fragments,
        })
    }
}

/// A prefix for renamed variables. A client can name its variables anything, including
/// `representations1`, so renamed variables take enough underscores to never meet one of
/// them.
fn renamed_variable_prefix(members: &[FlattenMember]) -> Option<String> {
    let mut client_variables = Vec::new();
    for member in members {
        let operation = member
            .fetch
            .operation
            .as_parsed()
            .ok()?
            .operations
            .iter()
            .next()?;
        for definition in &operation.variables {
            if !(member.fetch.kind.is_entities() && definition.name == "representations") {
                client_variables.push(definition.name.clone());
            }
        }
    }
    let mut prefix = String::new();
    while client_variables
        .iter()
        .any(|variable| variable.starts_with(&format!("{prefix}representations")))
    {
        prefix.push('_');
    }
    Some(prefix)
}

/// The variables and fragments of one member's operation whose names change in the shared
/// operation, and their new names.
struct Renames {
    variables: IndexMap<Name, Name>,
    fragments: IndexMap<Name, Name>,
}

impl Renames {
    fn new(
        index: usize,
        operation: &executable::Operation,
        document: &ExecutableDocument,
        is_entities: bool,
        prefix: &str,
    ) -> Self {
        let mut variables = IndexMap::default();
        if is_entities
            && operation
                .variables
                .iter()
                .any(|definition| definition.name == "representations")
        {
            variables.insert(
                Name::new_unchecked("representations"),
                Name::new_unchecked(&format!("{prefix}representations{index}")),
            );
        }
        let fragments = document
            .fragments
            .keys()
            .map(|name| {
                (
                    name.clone(),
                    Name::new_unchecked(&format!("{name}__{index}")),
                )
            })
            .collect();
        Self {
            variables,
            fragments,
        }
    }

    fn variable(&self, name: &Name) -> Name {
        self.variables.get(name).unwrap_or(name).clone()
    }

    fn selection_set(&self, selection_set: &mut SelectionSet) {
        for selection in &mut selection_set.selections {
            match selection {
                Selection::Field(field) => {
                    let field = field.make_mut();
                    for argument in &mut field.arguments {
                        self.value(&mut argument.make_mut().value);
                    }
                    self.directives(&mut field.directives);
                    self.selection_set(&mut field.selection_set);
                }
                Selection::InlineFragment(inline) => {
                    let inline = inline.make_mut();
                    self.directives(&mut inline.directives);
                    self.selection_set(&mut inline.selection_set);
                }
                Selection::FragmentSpread(spread) => {
                    let spread = spread.make_mut();
                    self.directives(&mut spread.directives);
                    if let Some(renamed) = self.fragments.get(&spread.fragment_name) {
                        spread.fragment_name = renamed.clone();
                    }
                }
            }
        }
    }

    fn directives(&self, directives: &mut executable::DirectiveList) {
        for directive in directives.iter_mut() {
            for argument in &mut directive.make_mut().arguments {
                self.value(&mut argument.make_mut().value);
            }
        }
    }

    fn value(&self, value: &mut Node<ast::Value>) {
        match value.make_mut() {
            ast::Value::Variable(name) => *name = self.variable(name),
            ast::Value::List(items) => items.iter_mut().for_each(|item| self.value(item)),
            ast::Value::Object(fields) => {
                fields.iter_mut().for_each(|(_, field)| self.value(field))
            }
            _ => {}
        }
    }
}

/// Replaces fragment spreads at the top of the operation with their selections, so the fields
/// under them can be aliased for this member alone.
fn inline_top_level_spreads(
    selection_set: &mut SelectionSet,
    document: &ExecutableDocument,
) -> Option<()> {
    for selection in &mut selection_set.selections {
        match selection {
            Selection::FragmentSpread(spread) => {
                let fragment = document.fragments.get(&spread.fragment_name)?;
                let mut inline = executable::InlineFragment::with_type_condition(
                    fragment.type_condition().clone(),
                );
                inline.selection_set = fragment.selection_set.clone();
                inline.directives = spread.directives.clone();
                inline_top_level_spreads(&mut inline.selection_set, document)?;
                *selection = Selection::InlineFragment(Node::new(inline));
            }
            Selection::InlineFragment(inline) => {
                inline_top_level_spreads(&mut inline.make_mut().selection_set, document)?;
            }
            Selection::Field(_) => {}
        }
    }
    Some(())
}

/// Aliases each top-level field, looking through inline fragments, and returns the keys of the
/// response with the keys they replace.
fn alias_top_level_fields(
    selection_set: &mut SelectionSet,
    alias: impl Fn(&Name) -> Option<Name> + Copy,
) -> Option<Vec<Renamed>> {
    let mut keys: IndexSet<(Name, Name)> = IndexSet::default();
    alias_fields(selection_set, alias, &mut keys)?;
    Some(
        keys.into_iter()
            .map(|(own, shared)| Renamed {
                own: own.as_str().into(),
                shared: shared.as_str().into(),
            })
            .collect(),
    )
}

fn alias_fields(
    selection_set: &mut SelectionSet,
    alias: impl Fn(&Name) -> Option<Name> + Copy,
    keys: &mut IndexSet<(Name, Name)>,
) -> Option<()> {
    for selection in &mut selection_set.selections {
        match selection {
            Selection::Field(field) => {
                let field = field.make_mut();
                let own = field.response_name().clone();
                let shared = alias(&own)?;
                field.alias = Some(shared.clone());
                keys.insert((own, shared));
            }
            Selection::InlineFragment(inline) => {
                alias_fields(&mut inline.make_mut().selection_set, alias, keys)?;
            }
            // Spreads at the top were inlined first.
            Selection::FragmentSpread(_) => return None,
        }
    }
    Some(())
}

/// The fragments `selection_set` spreads, directly or through other fragments. A fragment
/// spread only at the top of the operation is left out, since its fields were inlined.
fn spread_fragments(selection_set: &SelectionSet, document: &ExecutableDocument) -> IndexSet<Name> {
    fn visit(
        selection_set: &SelectionSet,
        document: &ExecutableDocument,
        spread: &mut IndexSet<Name>,
    ) {
        for selection in &selection_set.selections {
            match selection {
                Selection::Field(field) => visit(&field.selection_set, document, spread),
                Selection::InlineFragment(inline) => visit(&inline.selection_set, document, spread),
                Selection::FragmentSpread(fragment) => {
                    if spread.insert(fragment.fragment_name.clone())
                        && let Some(fragment) = document.fragments.get(&fragment.fragment_name)
                    {
                        visit(&fragment.selection_set, document, spread);
                    }
                }
            }
        }
    }
    let mut spread = IndexSet::default();
    visit(selection_set, document, &mut spread);
    spread
}

fn serialize_selections(selection_set: &SelectionSet) -> String {
    let selections: Vec<String> = selection_set
        .selections
        .iter()
        .map(|selection| selection.serialize().no_indent().to_string())
        .collect();
    selections.join(" ")
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use apollo_compiler::validation::Valid;
    use apollo_federation::query_plan::serializable_document::SerializableDocument;

    use super::*;
    use crate::json_ext::Path;
    use crate::query_planner::OperationKind;
    use crate::query_planner::fetch::EntitySelection;
    use crate::query_planner::fetch::FetchKind;

    const SCHEMA: &str = r#"
        scalar _Any
        union _Entity = Item | Box
        type Query {
          _entities(representations: [_Any!]!): [_Entity]!
          stock(warehouse: String): Int
          restock: Int
        }
        type Item { id: ID! pages(first: Int): Int! }
        type Box { id: ID! weight: Int! }
    "#;

    fn schema() -> Valid<Schema> {
        Schema::parse_and_validate(SCHEMA, "schema.graphql").expect("valid schema")
    }

    /// A member sending `operation` to the inventory subgraph. It is an entity fetch when the
    /// operation selects `_entities`.
    fn member(operation: &str) -> FlattenMember {
        let document =
            ExecutableDocument::parse_and_validate(&schema(), operation, "fetch.graphql")
                .expect("valid fetch operation");
        let kind = if operation.contains("_entities") {
            FetchKind::Entities(EntitySelection {
                requires: Vec::new(),
                input_rewrites: None,
                context_rewrites: None,
            })
        } else {
            FetchKind::Root
        };
        FlattenMember {
            paths: vec![Path::default()],
            fetch: FetchNode {
                service_name: "inventory".into(),
                kind,
                variable_usages: Vec::new(),
                operation: SerializableDocument::from_parsed(document),
                operation_name: None,
                operation_kind: OperationKind::Query,
                id: None,
                output_rewrites: None,
                schema_aware_hash: Default::default(),
                authorization: Default::default(),
            },
        }
    }

    /// The operation sending the members with `pieces` together, checked against the schema and
    /// indented to read.
    fn joined(pieces: &[&Arc<MemberPiece>]) -> String {
        let (operation, _) = join(pieces.iter().map(|piece| &***piece));
        ExecutableDocument::parse_and_validate(&schema(), &operation, "joined.graphql")
            .expect("the joined operation is valid")
            .to_string()
    }

    const ITEM_PAGES: &str = "
        query($representations: [_Any!]!, $first: Int) {
          _entities(representations: $representations) { ...Fields }
        }
        fragment Fields on Item { pages(first: $first) }";
    const STOCK: &str = "query($warehouse: String) { stock(warehouse: $warehouse) }";
    const BOX_WEIGHT: &str = "
        query($representations: [_Any!]!) {
          _entities(representations: $representations) { ...Fields }
        }
        fragment Fields on Box { weight }";

    #[test]
    fn each_member_gets_its_own_names() {
        let members = [member(ITEM_PAGES), member(STOCK), member(BOX_WEIGHT)];
        let pieces = BatchOperation::new(&members).expect("one subgraph").pieces;

        insta::assert_snapshot!(joined(&[&pieces[0], &pieces[1], &pieces[2]]), @r#"
        query($representations0: [_Any!]!, $first: Int, $warehouse: String, $representations2: [_Any!]!) {
          _entities0: _entities(representations: $representations0) {
            ...Fields__0
          }
          _r1_stock: stock(warehouse: $warehouse)
          _entities2: _entities(representations: $representations2) {
            ...Fields__2
          }
        }

        fragment Fields__0 on Item {
          pages(first: $first)
        }

        fragment Fields__2 on Box {
          weight
        }
        "#);
    }

    #[test]
    fn keys_and_variables_map_back_to_each_fetch() {
        let members = [member(ITEM_PAGES), member(STOCK)];
        let pieces = BatchOperation::new(&members).expect("one subgraph").pieces;

        let pairs = |renamed: &[Renamed]| -> Vec<(String, String)> {
            renamed
                .iter()
                .map(|name| (name.own.to_string(), name.shared.to_string()))
                .collect()
        };
        assert_eq!(
            pairs(&pieces[0].keys),
            [("_entities".into(), "_entities0".into())]
        );
        assert_eq!(
            pairs(&pieces[0].variables),
            [
                ("representations".into(), "representations0".into()),
                ("first".into(), "first".into()),
            ]
        );
        assert_eq!(
            pairs(&pieces[1].keys),
            [("stock".into(), "_r1_stock".into())]
        );
        assert_eq!(
            pairs(&pieces[1].variables),
            [("warehouse".into(), "warehouse".into())]
        );
    }

    #[test]
    fn a_request_sends_only_what_its_members_use() {
        let members = [member(ITEM_PAGES), member(STOCK), member(BOX_WEIGHT)];
        let pieces = BatchOperation::new(&members).expect("one subgraph").pieces;

        insta::assert_snapshot!(joined(&[&pieces[1], &pieces[2]]), @r#"
        query($warehouse: String, $representations2: [_Any!]!) {
          _r1_stock: stock(warehouse: $warehouse)
          _entities2: _entities(representations: $representations2) {
            ...Fields__2
          }
        }

        fragment Fields__2 on Box {
          weight
        }
        "#);
    }

    #[test]
    fn a_client_variable_is_defined_once() {
        let members = [
            member("query($warehouse: String) { stock(warehouse: $warehouse) }"),
            member("query($warehouse: String) { a: stock(warehouse: $warehouse) }"),
        ];
        let pieces = BatchOperation::new(&members).expect("one subgraph").pieces;

        let (_, variables) = join(pieces.iter().map(|piece| &**piece));
        assert_eq!(variables, ["warehouse"]);
    }

    #[test]
    fn representations_avoid_a_client_variable_with_their_name() {
        let members = [
            member(
                "query($representations: [_Any!]!) {
                   _entities(representations: $representations) { ... on Box { weight } }
                 }",
            ),
            member("query($representations0: String) { stock(warehouse: $representations0) }"),
        ];
        let pieces = BatchOperation::new(&members).expect("one subgraph").pieces;

        let (_, variables) = join(pieces.iter().map(|piece| &**piece));
        assert_eq!(variables, ["_representations0", "representations0"]);
    }

    #[test]
    fn fields_under_a_top_level_spread_are_aliased() {
        let members = [
            member("{ restock }"),
            member("{ ...Counts } fragment Counts on Query { stock restock }"),
        ];
        let pieces = BatchOperation::new(&members).expect("one subgraph").pieces;

        insta::assert_snapshot!(joined(&[&pieces[0], &pieces[1]]), @r#"
        {
          _r0_restock: restock
          ... on Query {
            _r1_stock: stock
            _r1_restock: restock
          }
        }
        "#);
    }

    #[test]
    fn the_operation_name_is_the_first_members() {
        let members = [
            member("query First { restock }"),
            member("query Second { stock }"),
        ];
        let pieces = BatchOperation::new(&members).expect("one subgraph").pieces;

        let (operation, _) = join([&*pieces[1], &*pieces[0]]);
        assert!(operation.starts_with("query Second {"), "{operation}");
    }
}
