use super::Assignment;
use super::Expression;
use super::ExpressionPath;
use crate::sources::connect::json_selection::lit_expr::LitExpr;
use crate::sources::connect::json_selection::KnownVariable;

#[allow(unused)]
pub(crate) fn lift_constants<'schema, 'selection>(
    assignments: Vec<Assignment<'schema, 'selection>>,
) -> Vec<Assignment<'schema, 'selection>> {
    assignments
        .into_iter()
        .map(|a| {
            let lifted = lift(a.right.clone());
            Assignment {
                left: a.left,
                right: lifted,
            }
        })
        .collect()
}

#[allow(unused)]
fn lift(expressions: ExpressionPath) -> ExpressionPath {
    // starting from the end, take each expression and stop after we encounter a variable or a literal expression
    let mut new_expressions = Vec::new();
    let mut encountered_stop = false;

    for expr in expressions.0.into_iter().rev() {
        if !encountered_stop {
            match &expr {
                Expression::KnownVariable(var) => match var {
                    KnownVariable::This | KnownVariable::Args | KnownVariable::Config => {
                        encountered_stop = true
                    }
                    // these are contextual, so we need the expressions before them
                    KnownVariable::Dollar => {}
                    KnownVariable::AtSign => {}
                },
                Expression::LitExpr(expr) => match expr {
                    LitExpr::String(_) | LitExpr::Number(_) | LitExpr::Bool(_) | LitExpr::Null => {
                        encountered_stop = true
                    }
                    LitExpr::Object(_) => {} // TODO: look for dynamic elements within
                    LitExpr::Array(_) => {}  // TODO: look for dynamic elements within
                    LitExpr::Path(_) => {}
                },
                _ => {}
            }
            new_expressions.push(expr);
        }
    }

    new_expressions.reverse();
    ExpressionPath(new_expressions)
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use insta::assert_debug_snapshot;

    use super::lift_constants;
    use crate::sources::connect::json_selection::single_assignment::AssignmentError;
    use crate::sources::connect::JSONSelection;

    #[test_log::test]
    fn test0() {
        let (_, s) = JSONSelection::parse(
            "
          $.root {
            a
            b: $(1)
            c: $({ a: 1, b: 2 })
            d: $args.a
            e: $this.b
          }
        ",
        )
        .unwrap();
        let schema = Schema::parse_and_validate(
            r#"
            type Query {
                f: T
            }

            type T {
                a: Int
                b: Int
                c: C
                d: Int
                e: Int
            }

            type C {
                a: Int
                b: Int
            }
            "#,
            "",
        )
        .unwrap();
        let parent_type = schema.types.get("Query").unwrap();
        let connector_field = schema.type_field("Query", "f").unwrap();
        let (assignments, errors) = s.single_assignment(&schema, (parent_type, connector_field));
        assert_eq!(errors, Vec::<AssignmentError>::new());
        assert_debug_snapshot!(assignments, @r###"
        [
            Assignment {
                left: Query.f: T | T.a: Int,
                right: $ | .root | .a,
            },
            Assignment {
                left: Query.f: T | T.b: Int,
                right: $ | .root | LitExpr(1) | LitExpr(1),
            },
            Assignment {
                left: Query.f: T | T.c: C | C.a: Int,
                right: $ | .root | LitExpr({"a": 1, "b": 2}) | .a | LitExpr(1),
            },
            Assignment {
                left: Query.f: T | T.c: C | C.b: Int,
                right: $ | .root | LitExpr({"a": 1, "b": 2}) | .b | LitExpr(2),
            },
            Assignment {
                left: Query.f: T | T.d: Int,
                right: $ | .root | $args | .a,
            },
            Assignment {
                left: Query.f: T | T.e: Int,
                right: $ | .root | $this | .b,
            },
        ]
        "###);

        let lifted = lift_constants(assignments);
        assert_debug_snapshot!(lifted, @r###"
        [
            Assignment {
                left: Query.f: T | T.a: Int,
                right: $ | .root | .a,
            },
            Assignment {
                left: Query.f: T | T.b: Int,
                right: LitExpr(1),
            },
            Assignment {
                left: Query.f: T | T.c: C | C.a: Int,
                right: LitExpr(1),
            },
            Assignment {
                left: Query.f: T | T.c: C | C.b: Int,
                right: LitExpr(2),
            },
            Assignment {
                left: Query.f: T | T.d: Int,
                right: $args | .a,
            },
            Assignment {
                left: Query.f: T | T.e: Int,
                right: $this | .b,
            },
        ]
        "###);
    }
}
