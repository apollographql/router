use std::num::NonZeroU32;

use apollo_federation::query_plan::query_planner::QueryPlannerConfig;
use apollo_federation::query_plan::query_planner::QueryPlannerDebugConfig;

/// Simple schema, created to force the query planner to have multiple choice. We'll build
/// a supergraph with the 2 _same_ subgraph having this exact same schema. In practice,
/// for every field `v_i`, the planner will consider the option of fetching it from either
/// the 1st or 2nd subgraph (not that in theory, there is more choices than this; we could
/// get `t.id` from the 1st subgraph and then jump to then 2nd subgraph, but some heuristics
/// in the the query planner recognize this is not useful. Also note that we currently
/// need both the `@key` on `T` and to have `Query.t` shareable for the query to consider
/// those choices).
const SUBGRAPH: &str = r#"
      type Query {
        t: T @shareable
      }

      type T @key(fields: "id") @shareable {
        id: ID!
        v1: Int
        v2: Int
        v3: Int
        v4: Int
      }
"#;

#[test]
fn works_when_unset() {
    // This test is mostly a sanity check to make sure that "by default", we do have 16 plans
    // (all combination of the 2 choices for 4 fields). It's not entirely impossible that
    // some future smarter heuristic is added to the planner so that it recognize it could
    // but the choices earlier, and if that's the case, this test will fail (showing that less
    // plans are considered) and we'll have to adapt the example (find a better way to force
    // choices).
    let planner = planner!(
        // default config
        Subgraph1: SUBGRAPH,
        Subgraph2: SUBGRAPH,
    );
    let plan = assert_plan!(
        &planner,
        r#"
          {
            t {
              v1
              v2
              v3
              v4
            }
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              t {
                v1
                v2
                v3
                v4
              }
            }
          },
        }
      "###
    );
    assert_eq!(plan.statistics.evaluated_plan_count.get(), 16);
}

#[test]
fn allows_setting_down_to_1() {
    let max_evaluated_plans = NonZeroU32::new(1).unwrap();
    let planner = planner!(
        config = QueryPlannerConfig {
            debug: QueryPlannerDebugConfig {
                max_evaluated_plans,
                ..Default::default()
            },
            ..Default::default()
        },
        Subgraph1: SUBGRAPH,
        Subgraph2: SUBGRAPH,
    );
    let plan = assert_plan!(
        &planner,
        r#"
          {
            t {
              v1
              v2
              v3
              v4
            }
          }
        "#,


      // Note that in theory, the planner would be excused if it wasn't generated this
      // (optimal in this case) plan. But we kind of want it in this simple example so
      // we still assert this is the plan we get.
      // Note2: `v1` ends up reordered in this case due to reordering of branches that
      // happens as a by-product of cutting out choice. This is completely harmless and
      // the plan is still find and optimal, but if we someday find the time to update
      // the code to keep the order more consistent (say, if we ever rewrite said code :)),
      // then this wouldn't be the worst thing either.
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              t {
                v2
                v3
                v4
                v1
              }
            }
          },
        }
      "###
    );
    assert_eq!(plan.statistics.evaluated_plan_count.get(), 1);
}

#[test]
fn can_be_set_to_an_arbitrary_number() {
    let max_evaluated_plans = NonZeroU32::new(10).unwrap();
    let planner = planner!(
        config = QueryPlannerConfig {
            debug: QueryPlannerDebugConfig {
                max_evaluated_plans,
                ..Default::default()
            },
            ..Default::default()
        },
        Subgraph1: SUBGRAPH,
        Subgraph2: SUBGRAPH,
    );
    let plan = assert_plan!(
        &planner,
        r#"
          {
            t {
              v1
              v2
              v3
              v4
            }
          }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              t {
                v1
                v4
                v2
                v3
              }
            }
          },
        }
      "###
    );

    // Note that in this particular example, since we have binary choices only and due to the way
    // we cut branches when we're above the max, the number of evaluated plans can only be a power
    // of 2. Here, we just want it to be the nearest power of 2 below our limit.
    assert_eq!(plan.statistics.evaluated_plan_count.get(), 8);
}

#[test]
fn cannot_be_set_to_0_or_a_negative_number() {
    // In JS this was explicit errors from the query planner.
    // In Rust these constraints are encoded in the type system
    // through config requiring a NonZeroU32:
    assert!(NonZeroU32::new(0).is_none());
    assert!(u32::try_from(-1).ok().and_then(NonZeroU32::new).is_none());
}

#[test]
fn correctly_generate_plan_built_from_some_non_individually_optimal_branch_options() {
    // The idea of this test is that the query has 2 leaf fields, `t.x` and `t.y`, whose
    // options are:
    //  1. `t.x`:
    //     a. S1(get t.x)
    //     b. S2(get t.id) -> S3(get t.x using key id)
    //  2. `t.y`:
    //     a. S2(get t.id) -> S3(get t.y using key id)
    //
    // And the idea is that "individually", for just `t.x`, getting it all in `S1` using option a.,
    // but for the whole plan, using option b. is actually better since it avoid querying `S1`
    // entirely (and `S2`/`S2` have to be queried anyway).
    //
    // Anyway, this test make sure we do correctly generate the plan using 1.b and 2.a, and do
    // not ignore 1.b in favor of 1.a in particular (which a bug did at one point).
    let planner = planner!(
        Subgraph1: r#"
        type Query {
          t: T @shareable
        }

        type T {
          x: Int @shareable
        }
        "#,
        Subgraph2: r#"
        type Query {
          t: T @shareable
        }

        type T @key(fields: "id") {
          id: ID!
        }
        "#,
        Subgraph3: r#"
        type T @key(fields: "id") {
          id: ID!
          x: Int @shareable
          y: Int
        }
        "#,
    );

    assert_plan!(
        &planner,
        r#"
        {
          t {
            x
            y
          }
        }
      "#,
        @r###"
         QueryPlan {
           Sequence {
             Fetch(service: "Subgraph2") {
               {
                 t {
                   __typename
                   id
                 }
               }
             },
             Flatten(path: "t") {
               Fetch(service: "Subgraph3") {
                 {
                   ... on T {
                     __typename
                     id
                   }
                 } =>
                 {
                   ... on T {
                     y
                     x
                   }
                 }
               },
             },
           },
         }
      "###
    );
}

#[test]
fn does_not_error_on_some_complex_fetch_group_dependencies() {
    // This test is a reproduction of a bug whereby planning on this example was raising an
    // assertion error due to an incorrect handling of fetch group dependencies.

    let planner = planner!(
        Subgraph1: r#"
        type Query {
          me: User @shareable
        }

        type User {
          id: ID! @shareable
        }
        "#,
        Subgraph2: r#"
        type Query {
          me: User @shareable
        }

        type User @key(fields: "id") {
          id: ID!
          p: Props
        }

        type Props {
          id: ID! @shareable
        }
        "#,
        Subgraph3: r#"
        type Query {
          me: User @shareable
        }

        type User {
          id: ID! @shareable
        }

        type Props @key(fields: "id") {
          id: ID!
          v0: Int
          t: T
        }

        type T {
          id: ID!
          v1: V
          v2: V

          # Note: this field is not queried, but matters to the reproduction this test exists
          # for because it prevents some optimizations that would happen without it (namely,
          # without it, the planner would notice that everything after type T is guaranteed
          # to be local to the subgraph).
          user: User
        }

        type V {
          x: Int
        }
        "#,
    );

    assert_plan!(
        &planner,
        r#"
        {
          me {
            p {
              v0
              t {
                v1 {
                  x
                }
                v2 {
                  x
                }
              }
            }
          }
        }
      "#,
        @r###"
      QueryPlan {
        Sequence {
          Fetch(service: "Subgraph2") {
            {
              me {
                p {
                  __typename
                  id
                }
              }
            }
          },
          Flatten(path: "me.p") {
            Fetch(service: "Subgraph3") {
              {
                ... on Props {
                  __typename
                  id
                }
              } =>
              {
                ... on Props {
                  v0
                  t {
                    v1 {
                      x
                    }
                    v2 {
                      x
                    }
                  }
                }
              }
            },
          },
        },
      }
    "###
    );
}

#[test]
fn does_not_evaluate_plans_relying_on_a_key_field_to_fetch_that_same_field() {
    let planner = planner!(
        Subgraph1: r#"
        type Query {
          t: T
        }

        type T @key(fields: "otherId") {
          otherId: ID!
        }
        "#,
        Subgraph2: r#"
        type T @key(fields: "id") @key(fields: "otherId") {
          id: ID!
          otherId: ID!
        }
        "#,
        Subgraph3: r#"
        type T @key(fields: "id") {
          id: ID!
        }
        "#,
    );

    let plan = assert_plan!(
        &planner,
        r#"
        {
          t {
            id
          }
        }
      "#,
        @r###"
      QueryPlan {
        Sequence {
          Fetch(service: "Subgraph1") {
            {
              t {
                __typename
                otherId
              }
            }
          },
          Flatten(path: "t") {
            Fetch(service: "Subgraph2") {
              {
                ... on T {
                  __typename
                  otherId
                }
              } =>
              {
                ... on T {
                  id
                }
              }
            },
          },
        },
      }
    "###
    );

    // This is the main thing this test exists for: making sure we only evaluate a
    // single plan for this example. And while it may be hard to see what other
    // plans than the one above could be evaluated, some older version of the planner
    // where considering a plan consisting of, from `Subgraph1`, fetching key `id`
    // in `Subgraph2` using key `otherId`, and then using that `id` key to fetch
    // ... field `id` in `Subgraph3`, not realizing that the `id` is what we ultimately
    // want and so there is no point in considering path that use it as key. Anyway
    // this test ensure this is not considered anymore (considering that later plan
    // was not incorrect, but it was adding to the options to evaluate which in some
    // cases could impact query planning performance quite a bit).
    assert_eq!(plan.statistics.evaluated_plan_count.get(), 1);
}

#[test]
fn avoid_considering_indirect_paths_from_the_root_when_a_more_direct_one_exists() {
    // Each of id/v0 can have 2 options each, so that's 4 combinations. If we were to consider 2 options for each
    // v1 value however, that would multiple it by 2 each times, so it would 32 possibilities. We limit the number of
    // evaluated plans just above our expected number of 4 so that if we exceed it, the generated plan will be sub-optimal.
    let max_evaluated_plans = NonZeroU32::new(6).unwrap();
    let planner = planner!(
        config = QueryPlannerConfig {
            debug: QueryPlannerDebugConfig {
                max_evaluated_plans,
                ..Default::default()
            },
            ..Default::default()
        },
        Subgraph1: r#"
        type Query {
          t: T @shareable
        }

        type T @key(fields: "id") {
          id: ID!
          v0: Int @shareable
        }
        "#,
        Subgraph2: r#"
        type Query {
          t: T @shareable
        }

        type T @key(fields: "id") {
          id: ID!
          v0: Int @shareable
          v1: Int
        }
        "#,
    );
    let plan = assert_plan!(
        &planner,
        r#"
        {
          t {
            id
            v0
            a0: v1
            a1: v1
            a2: v1
          }
        }
      "#,
        @r###"
      QueryPlan {
        Fetch(service: "Subgraph2") {
          {
            t {
              a0: v1
              a1: v1
              a2: v1
              id
              v0
            }
          }
        },
      }
    "###
    );

    // As said above, we legit have 2 options for `id` and `v0`, and we cannot know which are best before we evaluate the
    // plans completely. But for the multiple `v1`, we should recognize that going through the 1st subgraph (and taking a
    // key) is never exactly a good idea.
    assert_eq!(plan.statistics.evaluated_plan_count.get(), 4);
}

/// https://apollographql.atlassian.net/browse/FED-301
#[test]
fn multiplication_overflow_in_reduce_options_if_needed() {
    let planner = planner!(
        // default config
        Subgraph1: SUBGRAPH,
        Subgraph2: SUBGRAPH,
    );
    let plan = assert_plan!(
        &planner,
        r#"
        {
          t {
            f00: id  f01: id  f02: id  f03: id  f04: id  f05: id  f06: id  f07: id
            f08: id  f09: id  f10: id  f11: id  f12: id  f13: id  f14: id  f15: id
            f16: id  f17: id  f18: id  f19: id  f20: id  f21: id  f22: id  f23: id
            f24: id  f25: id  f26: id  f27: id  f28: id  f29: id  f30: id  f31: id
            f32: id  f33: id  f34: id  f35: id  f36: id  f37: id  f38: id  f39: id
            f40: id  f41: id  f42: id  f43: id  f44: id  f45: id  f46: id  f47: id
            f48: id  f49: id  f50: id  f51: id  f52: id  f53: id  f54: id  f55: id
            f56: id  f57: id  f58: id  f59: id  f60: id  f61: id  f62: id  f63: id
          }
        }
        "#,
        @r###"
        QueryPlan {
          Fetch(service: "Subgraph1") {
            {
              t {
                f14: id
                f15: id
                f16: id
                f17: id
                f18: id
                f19: id
                f20: id
                f21: id
                f22: id
                f23: id
                f24: id
                f25: id
                f26: id
                f27: id
                f28: id
                f29: id
                f30: id
                f31: id
                f32: id
                f33: id
                f34: id
                f35: id
                f36: id
                f37: id
                f38: id
                f39: id
                f40: id
                f41: id
                f42: id
                f43: id
                f44: id
                f45: id
                f46: id
                f47: id
                f48: id
                f49: id
                f50: id
                f51: id
                f52: id
                f53: id
                f54: id
                f55: id
                f56: id
                f57: id
                f58: id
                f59: id
                f60: id
                f61: id
                f62: id
                f63: id
                f00: id
                f13: id
                f01: id
                f02: id
                f03: id
                f04: id
                f05: id
                f06: id
                f07: id
                f08: id
                f09: id
                f10: id
                f11: id
                f12: id
              }
            }
          },
        }
        "###
    );
    // max_evaluated_plans defaults to 10_000
    assert_eq!(plan.statistics.evaluated_plan_count.get(), 8192);
}
