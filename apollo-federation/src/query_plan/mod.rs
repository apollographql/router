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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FetchNode {
    pub subgraph_name: Arc<str>,
    /// Optional identifier for the fetch for defer support. All fetches of a given plan will be
    /// guaranteed to have a unique `id`.
    pub id: Option<u64>,
    pub variable_usages: Vec<Name>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub requires: Vec<requires_selection::Selection>,
    // PORT_NOTE: We don't serialize the "operation" string in this struct, as these query plan
    // nodes are meant for direct consumption by router (without any serdes), so we leave the
    // question of whether it needs to be serialized to router.
    pub operation_document: serializable_document::SerializableDocument,
    pub operation_name: Option<Name>,
    #[serde(with = "crate::utils::serde_bridge::operation_type")]
    pub operation_kind: executable::OperationType,
    /// Optionally describe a number of "rewrites" that query plan executors should apply to the
    /// data that is sent as the input of this fetch. Note that such rewrites should only impact the
    /// inputs of the fetch they are applied to (meaning that, as those inputs are collected from
    /// the current in-memory result, the rewrite should _not_ impact said in-memory results, only
    /// what is sent in the fetch).
    pub input_rewrites: Arc<Vec<Arc<FetchDataRewrite>>>,
    /// Similar to `input_rewrites`, but for optional "rewrites" to apply to the data that is
    /// received from a fetch (and before it is applied to the current in-memory results).
    pub output_rewrites: Vec<Arc<FetchDataRewrite>>,
    /// Similar to the other kinds of rewrites. This is a mechanism to convert a contextual path into
    /// an argument to a resolver. Note value setters are currently unused here, but may be used in
    /// the future.
    pub context_rewrites: Vec<Arc<FetchDataRewrite>>,
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
    pub path: Vec<FetchDataPathElement>,
    pub node: Box<PlanNode>,
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
mod proptests {
    use std::cell::Cell;

    use proptest::prelude::*;
    use proptest::proptest;

    use super::*;

    fn plan_name_strategy() -> impl Strategy<Value = Name> {
        prop::sample::select(vec!["a", "b", "node", "items", "TypeA", "TypeB", "ctx"])
            .prop_map(|value| Name::new(value).unwrap())
    }

    fn conditions_strategy() -> impl Strategy<Value = Option<Conditions>> {
        prop::option::of(prop::collection::vec(plan_name_strategy(), 0..4))
    }

    fn fetch_data_path_strategy() -> impl Strategy<Value = Vec<FetchDataPathElement>> {
        prop::collection::vec(
            prop_oneof![
                (plan_name_strategy(), conditions_strategy())
                    .prop_map(|(name, conditions)| FetchDataPathElement::Key(name, conditions)),
                conditions_strategy().prop_map(FetchDataPathElement::AnyIndex),
                plan_name_strategy().prop_map(FetchDataPathElement::TypenameEquals),
                Just(FetchDataPathElement::Parent),
            ],
            0..7,
        )
    }

    fn query_path_strategy() -> impl Strategy<Value = Vec<QueryPathElement>> {
        prop::collection::vec(
            prop_oneof![
                plan_name_strategy()
                    .prop_map(|response_key| QueryPathElement::Field { response_key }),
                plan_name_strategy().prop_map(|type_condition| {
                    QueryPathElement::InlineFragment { type_condition }
                }),
            ],
            0..6,
        )
    }

    fn json_value_strategy() -> BoxedStrategy<serde_json_bytes::Value> {
        prop_oneof![
            Just(serde_json_bytes::Value::Null),
            any::<bool>().prop_map(serde_json_bytes::Value::Bool),
            any::<i64>().prop_map(|value| serde_json_bytes::Value::from(value)),
            "[a-zA-Z0-9 _-]{0,16}".prop_map(|value| serde_json_bytes::Value::String(value.into())),
        ]
        .prop_recursive(3, 32, 4, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..4).prop_map(serde_json_bytes::Value::Array),
                prop::collection::btree_map("[a-z]{1,8}", inner, 0..4).prop_map(|entries| {
                    serde_json_bytes::Value::Object(
                        entries
                            .into_iter()
                            .map(|(key, value)| (key.into(), value))
                            .collect(),
                    )
                }),
            ]
        })
        .boxed()
    }

    fn rewrite_strategy() -> impl Strategy<Value = Arc<FetchDataRewrite>> {
        prop_oneof![
            (fetch_data_path_strategy(), json_value_strategy()).prop_map(|(path, value)| {
                Arc::new(FetchDataRewrite::ValueSetter(FetchDataValueSetter {
                    path,
                    set_value_to: value,
                }))
            }),
            (fetch_data_path_strategy(), plan_name_strategy()).prop_map(|(path, rename_key_to)| {
                Arc::new(FetchDataRewrite::KeyRenamer(FetchDataKeyRenamer {
                    path,
                    rename_key_to,
                }))
            }),
        ]
    }

    fn requires_selection_strategy() -> BoxedStrategy<requires_selection::Selection> {
        (prop::option::of(plan_name_strategy()), plan_name_strategy())
            .prop_map(|(alias, name)| {
                requires_selection::Selection::Field(requires_selection::Field {
                    alias,
                    name,
                    selections: Vec::new(),
                })
            })
            .prop_recursive(3, 32, 4, |inner| {
                prop_oneof![
                    (
                        prop::option::of(plan_name_strategy()),
                        plan_name_strategy(),
                        prop::collection::vec(inner.clone(), 0..4),
                    )
                        .prop_map(|(alias, name, selections)| {
                            requires_selection::Selection::Field(requires_selection::Field {
                                alias,
                                name,
                                selections,
                            })
                        }),
                    (
                        prop::option::of(plan_name_strategy()),
                        prop::collection::vec(inner, 0..4),
                    )
                        .prop_map(|(type_condition, selections)| {
                            requires_selection::Selection::InlineFragment(
                                requires_selection::InlineFragment {
                                    type_condition,
                                    selections,
                                },
                            )
                        }),
                ]
            })
            .boxed()
    }

    fn fetch_node_strategy() -> BoxedStrategy<FetchNode> {
        (
            prop::sample::select(vec!["accounts", "products", "reviews"]),
            prop::option::of(any::<u64>()),
            prop::collection::vec(plan_name_strategy(), 0..4),
            prop::collection::vec(requires_selection_strategy(), 0..4),
            prop::option::of(plan_name_strategy()),
            prop::sample::select(vec![
                executable::OperationType::Query,
                executable::OperationType::Mutation,
                executable::OperationType::Subscription,
            ]),
            prop::collection::vec(rewrite_strategy(), 0..4),
            prop::collection::vec(rewrite_strategy(), 0..4),
            prop::collection::vec(rewrite_strategy(), 0..4),
        )
            .prop_map(
                |(
                    subgraph_name,
                    id,
                    variable_usages,
                    requires,
                    operation_name,
                    operation_kind,
                    input_rewrites,
                    output_rewrites,
                    context_rewrites,
                )| FetchNode {
                    subgraph_name: Arc::from(subgraph_name),
                    id,
                    variable_usages,
                    requires,
                    operation_document: serializable_document::SerializableDocument::from_string(
                        "query Generated { __typename }",
                    ),
                    operation_name,
                    operation_kind,
                    input_rewrites: Arc::new(input_rewrites),
                    output_rewrites,
                    context_rewrites,
                },
            )
            .boxed()
    }

    fn plan_node_strategy() -> BoxedStrategy<PlanNode> {
        fetch_node_strategy()
            .prop_map(|fetch| PlanNode::Fetch(Box::new(fetch)))
            .prop_recursive(4, 64, 4, |inner| {
                let optional_node =
                    || prop::option::of(inner.clone()).prop_map(|node| node.map(Box::new));
                prop_oneof![
                    prop::collection::vec(inner.clone(), 0..4)
                        .prop_map(|nodes| PlanNode::Sequence(SequenceNode { nodes })),
                    prop::collection::vec(inner.clone(), 0..4)
                        .prop_map(|nodes| PlanNode::Parallel(ParallelNode { nodes })),
                    (fetch_data_path_strategy(), inner.clone()).prop_map(|(path, node)| {
                        PlanNode::Flatten(FlattenNode {
                            path,
                            node: Box::new(node),
                        })
                    }),
                    (plan_name_strategy(), optional_node(), optional_node()).prop_map(
                        |(condition_variable, if_clause, else_clause)| {
                            PlanNode::Condition(Box::new(ConditionNode {
                                condition_variable,
                                if_clause,
                                else_clause,
                            }))
                        }
                    ),
                    (
                        prop::option::of(Just(".{ __typename }".to_string())),
                        optional_node(),
                        prop::collection::vec(
                            (
                                prop::collection::vec(any::<u16>(), 0..4),
                                prop::option::of(Just("generated-label".to_string())),
                                query_path_strategy(),
                                prop::option::of(Just(".{ __typename }".to_string())),
                                optional_node(),
                            ),
                            0..4,
                        ),
                    )
                        .prop_map(
                            |(primary_selection, primary_node, deferred)| {
                                PlanNode::Defer(DeferNode {
                                    primary: PrimaryDeferBlock {
                                        sub_selection: primary_selection,
                                        node: primary_node,
                                    },
                                    deferred: deferred
                                        .into_iter()
                                        .map(|(depends, label, query_path, sub_selection, node)| {
                                            DeferredDeferBlock {
                                                depends: depends
                                                    .into_iter()
                                                    .map(|id| DeferredDependency {
                                                        id: id.to_string(),
                                                    })
                                                    .collect(),
                                                label,
                                                query_path,
                                                sub_selection,
                                                node,
                                            }
                                        })
                                        .collect(),
                                })
                            }
                        ),
                ]
            })
            .boxed()
    }

    fn top_level_plan_node_strategy() -> BoxedStrategy<TopLevelPlanNode> {
        let plan = plan_node_strategy();
        prop_oneof![
            plan.clone().prop_map(|node| match node {
                PlanNode::Fetch(fetch) => TopLevelPlanNode::Fetch(fetch),
                PlanNode::Sequence(sequence) => TopLevelPlanNode::Sequence(sequence),
                PlanNode::Parallel(parallel) => TopLevelPlanNode::Parallel(parallel),
                PlanNode::Flatten(flatten) => TopLevelPlanNode::Flatten(flatten),
                PlanNode::Defer(defer) => TopLevelPlanNode::Defer(defer),
                PlanNode::Condition(condition) => TopLevelPlanNode::Condition(condition),
            }),
            (fetch_node_strategy(), prop::option::of(plan)).prop_map(|(primary, rest)| {
                TopLevelPlanNode::Subscription(SubscriptionNode {
                    primary: Box::new(primary),
                    rest: rest.map(Box::new),
                })
            }),
        ]
        .boxed()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// The serialized plan AST is a public boundary consumed by the router. Generate every
        /// node/path/rewrite family recursively and require serialization to be stable after a
        /// deserialize-reserialize cycle. Statistics' documented NaN/null normalization is checked
        /// separately because IEEE NaN is intentionally not equal to itself.
        #[test]
        fn query_plan_json_round_trip_is_stable(
            node in prop::option::of(top_level_plan_node_strategy()),
            evaluated_plan_count in 0usize..10_000,
            evaluated_plan_paths in 0usize..10_000,
            best_plan_cost in prop_oneof![Just(f64::NAN), -1.0e9f64..1.0e9f64],
        ) {
            let plan = QueryPlan {
                node,
                statistics: QueryPlanningStatistics {
                    evaluated_plan_count: Cell::new(evaluated_plan_count),
                    evaluated_plan_paths: Cell::new(evaluated_plan_paths),
                    best_plan_cost,
                },
            };
            let serialized = serde_json::to_vec(&plan).unwrap();
            let round_trip: QueryPlan = serde_json::from_slice(&serialized).unwrap();
            let reserialized = serde_json::to_vec(&round_trip).unwrap();

            prop_assert_eq!(reserialized, serialized, "query-plan JSON was not stable");
            prop_assert_eq!(&round_trip.node, &plan.node);
            prop_assert_eq!(
                round_trip.statistics.evaluated_plan_count.get(),
                evaluated_plan_count,
            );
            prop_assert_eq!(
                round_trip.statistics.evaluated_plan_paths.get(),
                evaluated_plan_paths,
            );
            if best_plan_cost.is_nan() {
                prop_assert!(round_trip.statistics.best_plan_cost.is_nan());
            } else {
                prop_assert_eq!(round_trip.statistics.best_plan_cost, best_plan_cost);
            }
        }
    }
}
