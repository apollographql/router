//! Cooperative cancellation

use std::cell::Cell;
use std::ops::ControlFlow;

use apollo_compiler::ExecutableDocument;
use apollo_federation::error::FederationError;
use apollo_federation::error::SingleFederationError;
use apollo_federation::query_plan::QueryPlan;
use apollo_federation::query_plan::query_planner::QueryPlanOptions;

macro_rules! plan_with_check {
    ($check_for_cooperative_cancellation:expr) => {
        run_planner_with_check(
            $check_for_cooperative_cancellation,
            planner!(
                Subgraph1: r#"
                type Query {
                    t: T
                }

                type T @key(fields: "id") {
                    id: ID!
                    x: Int
                }
                "#
            )
        )
    };
}

fn run_planner_with_check(
    check_for_cooperative_cancellation: &dyn Fn() -> ControlFlow<()>,
    planner: apollo_federation::query_plan::query_planner::QueryPlanner,
) -> Result<QueryPlan, FederationError> {
    let api_schema = planner.api_schema();
    let doc = r#"
      query {
        t {
          __typename
          x
        }
      }
    "#;
    let doc = ExecutableDocument::parse_and_validate(api_schema.schema(), doc, "").unwrap();
    planner.build_query_plan(
        &doc,
        None,
        QueryPlanOptions {
            check_for_cooperative_cancellation: Some(check_for_cooperative_cancellation),
            ..Default::default()
        },
    )
}

#[track_caller]
fn assert_cancelled(result: Result<QueryPlan, FederationError>) {
    match result {
        Err(FederationError::SingleFederationError(SingleFederationError::PlanningCancelled)) => {}
        Err(e) => panic!("expected PlanningCancelled error, got {e}"),
        Ok(_) => panic!("expected PlanningCancelled, got a successful query plan"),
    }
}

#[test]
fn test_callback_is_called() {
    let counter = Cell::new(0);
    let result = plan_with_check!(&|| {
        counter.set(counter.get() + 1);
        ControlFlow::Continue(())
    });
    assert!(result.is_ok());
    // The actual count was 9 when this test was first written.
    // We don’t assert an exact number because it changing as the planner evolves is fine.
    assert!(counter.get() > 5);
}

#[test]
fn test_cancel_as_soon_as_possible() {
    let counter = Cell::new(0);
    let result = plan_with_check!(&|| {
        counter.set(counter.get() + 1);
        ControlFlow::Break(())
    });
    assert_cancelled(result);
    assert_eq!(counter.get(), 1);
}

#[test]
fn test_cancel_near_the_middle() {
    let counter = Cell::new(0);
    let result = plan_with_check!(&|| {
        counter.set(counter.get() + 1);
        if counter.get() == 5 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    assert_cancelled(result);
    assert_eq!(counter.get(), 5);
}

#[test]
fn test_cancel_late_enough_that_planning_finishes() {
    let counter = Cell::new(0);
    let result = plan_with_check!(&|| {
        counter.set(counter.get() + 1);
        if counter.get() >= 1_000 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    assert!(result.is_ok());
}
