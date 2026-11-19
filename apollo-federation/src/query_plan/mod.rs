use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::executable;
use serde::Deserialize;
use serde::Serialize;

use crate::query_plan::query_planner::QueryPlanningStatistics;

pub(crate) mod conditions;
pub(crate) mod display;
pub(crate) mod fetch_dependency_graph;
pub(crate) mod fetch_dependency_graph_processor;
pub mod generate;
pub mod incremental_planner;
pub mod query_planner;
pub(crate) mod query_planning_traversal;
pub mod requires_selection;
pub mod serializable_document;

pub type QueryPlanCost = f64;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QueryPlan {
    pub node: Option<TopLevelPlanNode>,
    pub statistics: QueryPlanningStatistics,
}

#[derive(Debug, Clone, PartialEq, derive_more::From, Serialize, Deserialize)]
pub enum TopLevelPlanNode {
    Subscription(SubscriptionNode),
    #[from(FetchNode, Box<FetchNode>)]
    Fetch(Box<FetchNode>),
    Sequence(SequenceNode),
    Parallel(ParallelNode),
    Flatten(FlattenNode),
    Defer(DeferNode),
    #[from(ConditionNode, Box<ConditionNode>)]
    Condition(Box<ConditionNode>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscriptionNode {
    pub primary: Box<FetchNode>,
    // XXX(@goto-bus-stop) Is this not just always a SequenceNode?
    pub rest: Option<Box<PlanNode>>,
}

#[derive(Debug, Clone, PartialEq, derive_more::From, Serialize, Deserialize)]
pub enum PlanNode {
    #[from(FetchNode, Box<FetchNode>)]
    Fetch(Box<FetchNode>),
    Sequence(SequenceNode),
    Parallel(ParallelNode),
    Flatten(FlattenNode),
    Defer(DeferNode),
    #[from(ConditionNode, Box<ConditionNode>)]
    Condition(Box<ConditionNode>),
}

/// How a fetch communicates with its data source. Extensible for future
/// non-GraphQL protocols (gRPC, SQL, etc.).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum FetchProtocol {
    /// Standard GraphQL subgraph fetch.
    #[default]
    GraphQL,
    /// Connector-based fetch (REST, etc.). The coordinate string identifies
    /// the connector (e.g., `subgraph:Type.field[0]`) for execution-layer
    /// lookup.
    Connector { coordinate: String },
}

impl FetchProtocol {
    pub fn is_graphql(&self) -> bool {
        matches!(self, FetchProtocol::GraphQL)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FetchNode {
    pub subgraph_name: Arc<str>,
    /// Protocol used to execute this fetch. Defaults to GraphQL.
    #[serde(default)]
    #[serde(skip_serializing_if = "FetchProtocol::is_graphql")]
    pub protocol: FetchProtocol,
    /// Optional identifier for the fetch for defer support. All fetches of a given plan will be
    /// guaranteed to have a unique `id`.
    pub id: Option<u64>,
    pub variable_usages: Vec<Name>,
    /// Root or entity fetch, with what it sends from data already in the response. Serialized
    /// as `fetch_kind`, since the kind inside it is tagged `kind`.
    #[serde(rename = "fetch_kind")]
    pub kind: FetchKind,
    // PORT_NOTE: We don't serialize the "operation" string in this struct, as these query plan
    // nodes are meant for direct consumption by router (without any serdes), so we leave the
    // question of whether it needs to be serialized to router.
    pub operation_document: serializable_document::SerializableDocument,
    pub operation_name: Option<Name>,
    #[serde(with = "crate::utils::serde_bridge::operation_type")]
    pub operation_kind: executable::OperationType,
    /// Similar to `EntitySelection::input_rewrites`, but for optional "rewrites" to apply to the
    /// data that is received from a fetch (and before it is applied to the current in-memory
    /// results).
    pub output_rewrites: Vec<Arc<FetchDataRewrite>>,
}

/// Whether a fetch selects root fields or `_entities`, and what it sends from data already in
/// the response to do so.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum FetchKind {
    /// Fields of a root operation type.
    Root,
    /// One `_entities` selection under `$representations`.
    Entities(EntitySelection),
}

/// The `_entities` selection of an entity fetch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntitySelection {
    /// The selection that builds each representation from an object in the response.
    pub requires: Vec<requires_selection::Selection>,
    /// Optionally describe a number of "rewrites" that query plan executors should apply to the
    /// data that is sent as the input of this selection. Note that such rewrites should only
    /// impact the inputs of the fetch they are applied to (meaning that, as those inputs are
    /// collected from the current in-memory result, the rewrite should _not_ impact said
    /// in-memory results, only what is sent in the fetch).
    pub input_rewrites: Arc<Vec<Arc<FetchDataRewrite>>>,
    /// Similar to the other kinds of rewrites. This is a mechanism to convert a contextual path into
    /// an argument to a resolver. Note value setters are currently unused here, but may be used in
    /// the future.
    pub context_rewrites: Vec<Arc<FetchDataRewrite>>,
}

impl FetchKind {
    /// The fetch's `_entities` selection, if it is an entity fetch.
    pub fn entity_selection(&self) -> Option<&EntitySelection> {
        match self {
            Self::Root => None,
            Self::Entities(selection) => Some(selection),
        }
    }

    /// The context rewrites of an entity fetch, and none for a root fetch.
    pub fn context_rewrites(&self) -> &[Arc<FetchDataRewrite>] {
        self.entity_selection()
            .map_or(&[], |selection| &selection.context_rewrites)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SequenceNode {
    pub nodes: Vec<PlanNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelNode {
    pub nodes: Vec<PlanNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlattenNode {
    pub members: Vec<FlattenMember>,
}

/// One fetch of a `FlattenNode`. An entity fetch collects its representations at every one of
/// its paths.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlattenMember {
    pub paths: Vec<Vec<FetchDataPathElement>>,
    pub fetch: FetchNode,
}

impl FlattenNode {
    /// `fetch` with its whole response merging at `path`.
    pub fn at_path(path: Vec<FetchDataPathElement>, fetch: FetchNode) -> Self {
        Self {
            members: vec![FlattenMember {
                paths: vec![path],
                fetch,
            }],
        }
    }

    /// The fetch and the path it merges at, when the node is one fetch at one path.
    pub fn single(&self) -> Option<(&[FetchDataPathElement], &FetchNode)> {
        let [member] = self.members.as_slice() else {
            return None;
        };
        let [path] = member.paths.as_slice() else {
            return None;
        };
        Some((path.as_slice(), &member.fetch))
    }

    /// The fetches this node stands for, each on its own at one path. For tools that reason
    /// about individual fetches.
    pub fn batched_fetches(&self) -> Vec<PlanNode> {
        self.members
            .iter()
            .flat_map(|member| {
                member.paths.iter().map(|path| {
                    PlanNode::Flatten(Self::at_path(path.clone(), member.fetch.clone()))
                })
            })
            .collect()
    }
}

/// A `DeferNode` corresponds to one or more `@defer` applications at the same level of "nestedness"
/// in the planned query.
///
/// It contains a "primary block" and a vector of "deferred blocks". The "primary block" represents
/// the part of the query that is _not_ deferred (so the part of the query up until we reach the
/// @defer(s) this handles), while each "deferred block" correspond to the deferred part of one of
/// the @defer(s) handled by the node.
///
/// Note that `DeferNode`s are only generated if defer support is enabled for the query planner.
/// Also note that if said support is enabled, then `DeferNode`s are always generated if the query
/// has a @defer application, even if in some cases generated plan may not "truly" defer the
/// underlying fetches (i.e. in cases where `deferred[*].node` are all undefined). This currently
/// happens because some specific cases of defer cannot be handled, but could later also happen if
/// we implement more advanced server-side heuristics to decide if deferring is judicious or not.
/// This allows the executor of the plan to consistently send a defer-abiding multipart response to
/// the client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeferNode {
    /// The "primary" part of a defer, that is the non-deferred part (though could be deferred
    /// itself for a nested defer).
    pub primary: PrimaryDeferBlock,
    /// The "deferred" parts of the defer (note that it's a vector). Each of those deferred elements
    /// will correspond to a different chunk of the response to the client (after the initial
    /// on-deferred one that is).
    pub deferred: Vec<DeferredDeferBlock>,
}

/// The primary block of a `DeferNode`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrimaryDeferBlock {
    /// The part of the original query that "selects" the data to send in that primary response
    /// once the plan in `node` completes). Note that if the parent `DeferNode` is nested, then it
    /// must come inside the `DeferredNode` in which it is nested, and in that case this
    /// sub-selection will start at that parent `DeferredNode.query_path`. Note that this can be
    /// `None` in the rare case that everything in the original query is deferred (which is not very
    /// useful  in practice, but not disallowed by the @defer spec at the moment).
    pub sub_selection: Option<String>,
    /// The plan to get all the data for the primary block. Same notes as for subselection: usually
    /// defined, but can be undefined in some corner cases where nothing is to be done in the
    /// primary block.
    pub node: Option<Box<PlanNode>>,
}

/// A deferred block of a `DeferNode`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeferredDeferBlock {
    /// References one or more fetch node(s) (by `id`) within `DeferNode.primary.node`. The plan of
    /// this deferred part should not be started until all such fetches return.
    pub depends: Vec<DeferredDependency>,
    /// The optional defer label.
    pub label: Option<String>,
    /// Path, in the query, to the `@defer` application this corresponds to. The `sub_selection`
    /// starts at this `query_path`.
    pub query_path: Vec<QueryPathElement>,
    /// The part of the original query that "selects" the data to send in the deferred response
    /// (once the plan in `node` completes). Will be set _unless_ `node` is a `DeferNode` itself.
    pub sub_selection: Option<String>,
    /// The plan to get all the data for this deferred block. Usually set, but can be `None` for a
    /// `@defer` application where everything has been fetched in the "primary block" (i.e. when
    /// this deferred block only exists to expose what should be send to the upstream client in a
    /// deferred response), but without declaring additional fetches. This happens for @defer
    /// applications that cannot be handled through the query planner and where the defer cannot be
    /// passed through to the subgraph).
    pub node: Option<Box<PlanNode>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeferredDependency {
    /// A `FetchNode` ID.
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionNode {
    pub condition_variable: Name,
    pub if_clause: Option<Box<PlanNode>>,
    pub else_clause: Option<Box<PlanNode>>,
}

/// The type of rewrites currently supported on the input/output data of fetches.
///
/// A rewrite usually identifies some sub-part of the data and some action to perform on that
/// sub-part.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, derive_more::From)]
pub enum FetchDataRewrite {
    ValueSetter(FetchDataValueSetter),
    KeyRenamer(FetchDataKeyRenamer),
}

/// A rewrite that sets a value at the provided path of the data it is applied to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FetchDataValueSetter {
    /// Path to the value that is set by this "rewrite".
    pub path: Vec<FetchDataPathElement>,
    /// The value to set at `path`. Note that the query planner currently only uses string values,
    /// but that may change in the future.
    pub set_value_to: serde_json_bytes::Value,
}

/// A rewrite that renames the key at the provided path of the data it is applied to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FetchDataKeyRenamer {
    /// Path to the key that is renamed by this "rewrite".
    pub path: Vec<FetchDataPathElement>,
    /// The key to rename to at `path`.
    pub rename_key_to: Name,
}

/// Vectors of this element match path(s) to a value in fetch data. Each element is (1) a key in
/// object data, (2) _any_ index in array data (often serialized as `@`), (3) a typename constraint
/// on the object data at that point in the path(s) (a path should only match for objects whose
/// `__typename` is the provided type), or (4) a parent indicator to move upwards one level in the
/// object.
///
/// It's possible for vectors of this element to match no paths in fetch data, e.g. if an object key
/// doesn't exist, or if an object's `__typename` doesn't equal the provided one. If this occurs,
/// then query plan execution should not execute the instruction this path is associated with.
///
/// The path starts at the top of the data it is applied to. So for instance, for fetch data inputs,
/// the path starts at the root of the object representing those inputs.
///
/// Note that the `@` is currently optional in some contexts, as query plan execution may assume
/// upon encountering array data in a path that it should match the remaining path to the array's
/// elements.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FetchDataPathElement {
    Key(Name, Option<Conditions>),
    AnyIndex(Option<Conditions>),
    TypenameEquals(Name),
    Parent,
}

pub type Conditions = Vec<Name>;

/// Vectors of this element match a path in a query. Each element is (1) a field in a query, or (2)
/// an inline fragment in a query.
#[derive(Debug, Clone, PartialEq, serde::Serialize, Deserialize)]
pub enum QueryPathElement {
    Field { response_key: Name },
    InlineFragment { type_condition: Name },
}

impl PlanNode {
    /// Returns the kind of plan node this is as a human-readable string. Exact output not guaranteed.
    fn node_kind(&self) -> &'static str {
        match self {
            Self::Fetch(_) => "Fetch",
            Self::Sequence(_) => "Sequence",
            Self::Parallel(_) => "Parallel",
            Self::Flatten(_) => "Flatten",
            Self::Defer(_) => "Defer",
            Self::Condition(_) => "Condition",
        }
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;

    use super::*;

    fn item() -> Vec<FetchDataPathElement> {
        vec![FetchDataPathElement::Key(name!("item"), None)]
    }

    fn fetch(kind: FetchKind) -> FetchNode {
        FetchNode {
            subgraph_name: "inventory".into(),
            protocol: FetchProtocol::GraphQL,
            id: None,
            variable_usages: Vec::new(),
            kind,
            operation_document: serializable_document::SerializableDocument::from_string(""),
            operation_name: None,
            operation_kind: executable::OperationType::Query,
            output_rewrites: Vec::new(),
        }
    }

    fn entity_fetch() -> FetchNode {
        fetch(FetchKind::Entities(EntitySelection {
            requires: vec![requires_selection::Selection::Field(
                requires_selection::Field {
                    alias: None,
                    name: name!("id"),
                    selections: Vec::new(),
                },
            )],
            input_rewrites: Default::default(),
            context_rewrites: Vec::new(),
        }))
    }

    /// `single()` only answers for exactly one member at exactly one path.
    #[test]
    fn single_requires_one_member_at_one_path() {
        let member = |paths| FlattenMember {
            paths,
            fetch: entity_fetch(),
        };
        let cases = [
            (vec![member(vec![item()])], Some(1)),
            (vec![member(vec![item()]), member(vec![item()])], None),
            (vec![member(vec![item(), item()])], None),
            (Vec::new(), None),
        ];
        for (members, single_path_len) in cases {
            let flatten = FlattenNode { members };
            assert_eq!(
                flatten.single().map(|(path, _)| path.len()),
                single_path_len
            );
            let fetch_count: usize = flatten.members.iter().map(|m| m.paths.len()).sum();
            assert_eq!(flatten.batched_fetches().len(), fetch_count);
        }
    }

    /// A fetch's kind is written out, so reading a plan never guesses it from `requires`.
    #[test]
    fn fetch_kind_is_explicit_in_json() {
        let entity = serde_json::to_value(entity_fetch()).expect("serializes");
        assert_eq!(entity["fetch_kind"]["kind"], "Entities");
        assert_eq!(entity["fetch_kind"]["requires"][0]["name"], "id");
        assert!(entity.get("requires").is_none());
        let read: FetchNode = serde_json::from_value(entity).expect("deserializes");
        assert_eq!(read, entity_fetch());

        let root = serde_json::to_value(fetch(FetchKind::Root)).expect("serializes");
        assert_eq!(root["fetch_kind"], serde_json::json!({ "kind": "Root" }));
        let read: FetchNode = serde_json::from_value(root).expect("deserializes");
        assert_eq!(read.kind, FetchKind::Root);
    }
}
