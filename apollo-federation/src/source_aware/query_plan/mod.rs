use std::sync::Arc;

use apollo_compiler::executable::Field;
use apollo_compiler::executable::InlineFragment;
use apollo_compiler::executable::Name;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::NodeStr;
use indexmap::IndexSet;

use crate::sources::source;

mod query_planner;

pub type QueryPlanCost = f64;

#[derive(Debug, Default)]
pub struct QueryPlan {
    pub node: Option<TopLevelPlanNode>,
}

impl QueryPlan {
    fn new(node: impl Into<TopLevelPlanNode>) -> Self {
        Self {
            node: Some(node.into()),
        }
    }
}

#[derive(Debug, derive_more::From)]
pub enum TopLevelPlanNode {
    Subscription(SubscriptionNode),
    Fetch(FetchNode),
    Sequence(SequenceNode),
    Parallel(ParallelNode),
    Flatten(FlattenNode),
    Defer(DeferNode),
    Condition(ConditionNode),
}

#[derive(Debug)]
pub struct SubscriptionNode {
    pub primary: FetchNode,
    pub rest: Option<PlanNode>,
}

#[derive(Clone, Debug)]
pub enum PlanNode {
    Fetch(Arc<FetchNode>),
    Sequence(Arc<SequenceNode>),
    Parallel(Arc<ParallelNode>),
    Flatten(Arc<FlattenNode>),
    Defer(Arc<DeferNode>),
    Condition(Arc<ConditionNode>),
}

#[derive(Debug)]
pub struct FetchNode {
    pub operation_variables: IndexSet<Name>,
    pub input_conditions: SelectionSet,
    pub source_data: source::query_plan::FetchNode,
}

#[derive(Debug)]
pub struct SequenceNode {
    pub nodes: Vec<PlanNode>,
}

#[derive(Debug)]
pub struct ParallelNode {
    pub nodes: Vec<PlanNode>,
}

#[derive(Debug)]
pub struct FlattenNode {
    pub path: Vec<FetchDataPathElement>,
    pub node: PlanNode,
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
#[derive(Debug)]
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
#[derive(Debug)]
pub struct PrimaryDeferBlock {
    /// The part of the original query that "selects" the data to send in that primary response
    /// once the plan in `node` completes). Note that if the parent `DeferNode` is nested, then it
    /// must come inside the `DeferredNode` in which it is nested, and in that case this
    /// sub-selection will start at that parent `DeferredNode.query_path`. Note that this can be
    /// `None` in the rare case that everything in the original query is deferred (which is not very
    /// useful  in practice, but not disallowed by the @defer spec at the moment).
    pub sub_selection: Option<SelectionSet>,
    /// The plan to get all the data for the primary block. Same notes as for subselection: usually
    /// defined, but can be undefined in some corner cases where nothing is to be done in the
    /// primary block.
    pub node: Option<PlanNode>,
}

/// A deferred block of a `DeferNode`.
#[derive(Debug)]
pub struct DeferredDeferBlock {
    /// References one or more fetch node(s) (by `id`) within `DeferNode.primary.node`. The plan of
    /// this deferred part should not be started until all such fetches return.
    pub depends: Vec<DeferredDependency>,
    /// The optional defer label.
    pub label: Option<NodeStr>,
    /// Path, in the query, to the `@defer` application this corresponds to. The `sub_selection`
    /// starts at this `query_path`.
    pub query_path: Vec<QueryPathElement>,
    /// The part of the original query that "selects" the data to send in the deferred response
    /// (once the plan in `node` completes). Will be set _unless_ `node` is a `DeferNode` itself.
    pub sub_selection: Option<SelectionSet>,
    /// The plan to get all the data for this deferred block. Usually set, but can be `None` for a
    /// `@defer` application where everything has been fetched in the "primary block" (i.e. when
    /// this deferred block only exists to expose what should be send to the upstream client in a
    /// deferred response), but without declaring additional fetches. This happens for @defer
    /// applications that cannot be handled through the query planner and where the defer cannot be
    /// passed through to the subgraph).
    pub node: Option<PlanNode>,
}

#[derive(Debug)]
pub struct DeferredDependency {
    /// A `FetchNode` ID.
    pub id: NodeStr,
}

#[derive(Debug)]
pub struct ConditionNode {
    pub condition_variable: Name,
    pub if_clause: Option<PlanNode>,
    pub else_clause: Option<PlanNode>,
}

/// Vectors of this element match path(s) to a value in fetch data. Each element is (1) a key in
/// object data, (2) _any_ index in array data (often serialized as `@`), or (3) a typename
/// constraint on the object data at that point in the path(s) (a path should only match for objects
/// whose `__typename` is the provided type).
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
#[derive(Debug, Clone, PartialEq)]
pub enum FetchDataPathElement {
    Key(NodeStr),
    AnyIndex,
    TypenameEquals(NodeStr),
}

/// Vectors of this element match a path in a query. Each element is (1) a field in a query, or (2)
/// an inline fragment in a query.
#[derive(Debug, Clone)]
pub enum QueryPathElement {
    Field(Field),
    InlineFragment(InlineFragment),
}
