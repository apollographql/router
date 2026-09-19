//! Differential and 3-way comparison harness for GraphQL query inclusion.
//!
//! Three implementations of the same predicate are compared:
//!
//! | lane | implementation | role |
//! | --- | --- | --- |
//! | Lean | `QueryInclusion.includesBool` | the model, with soundness and completeness theorems |
//! | `query_compare` | `apollo_federation::correctness::query_compare::includes` | the port under test |
//! | `response_shape` | `apollo_federation::correctness::compare_operations` | an independent algorithm |
//!
//! Lean against `query_compare` is the strict equality check: the port claims to compute exactly
//! what the model computes, so any disagreement is a defect in one of them. The response-shape
//! lane is advisory — it decides the same predicate by a different route, and the two are not
//! claimed to be equally complete, so its disagreements are reported and counted rather than
//! failed on.

pub mod harness;
pub mod lean_oracle;
pub mod model;
pub mod plan_fixture;
pub mod plan_model;
pub mod plan_oracle;
pub mod properties;
