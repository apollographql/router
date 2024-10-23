use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::Name;
use apollo_compiler::Schema;
use itertools::Itertools;

use super::lit_expr::LitExpr;
use super::location::WithRange;
use super::JSONSelection;
use super::Key;
use super::KnownVariable;
use super::MethodArgs;
use super::NamedSelection;
use super::PathList;
use super::PathSelection;
use super::SubSelection;
use crate::sources::connect::json_selection::PrettyPrintable;

// --- Assignment ------------------------------------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) struct Assignment<'schema, 'selection> {
    #[allow(unused)]
    left: FieldPath<'schema>,
    #[allow(unused)]
    right: ExpressionPath<'selection>,
}

// --- Left ------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ExtendedCompositeType<'schema>(&'schema ExtendedType);

impl<'schema> ExtendedCompositeType<'schema> {
    fn name(&self) -> &Name {
        self.0.name()
    }

    fn field(&self, name: &str) -> Option<&'schema Component<FieldDefinition>> {
        let Ok(name) = Name::new(name) else {
            return None;
        };
        match &self.0 {
            ExtendedType::Object(obj) => obj.fields.get(&name),
            ExtendedType::Interface(int) => int.fields.get(&name),
            _ => None,
        }
    }
}

impl<'schema> TryFrom<&'schema ExtendedType> for ExtendedCompositeType<'schema> {
    type Error = ();

    fn try_from(ty: &'schema ExtendedType) -> Result<Self, Self::Error> {
        match ty {
            ExtendedType::Object(_) | ExtendedType::Interface(_) => Ok(Self(ty)),
            _ => Err(()),
        }
    }
}

#[derive(Clone)]
pub(crate) struct FieldWithParent<'schema> {
    def: &'schema Component<FieldDefinition>,
    parent: ExtendedCompositeType<'schema>,
}

impl<'schema> std::fmt::Debug for FieldWithParent<'schema> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}.{}: {}",
            self.parent.name(),
            self.def.name,
            self.def.ty
        )
    }
}

#[derive(Clone)]
pub(crate) struct FieldPath<'schema>(Vec<FieldWithParent<'schema>>);

impl<'schema> FieldPath<'schema> {
    fn add(&self, field_with_parent: FieldWithParent<'schema>) -> Self {
        let mut new = self.clone();
        new.0.push(field_with_parent);
        new
    }

    fn next_parent_type(&self, schema: &'schema Valid<Schema>) -> ExtendedCompositeType<'schema> {
        let output_named_type_name = self.0.last().unwrap().def.ty.inner_named_type();
        let output_named_type = schema.types.get(output_named_type_name).unwrap();
        let composite_type: ExtendedCompositeType = output_named_type
            .try_into()
            .expect("Field selections should only be applied to objects or interfaces");
        composite_type
    }

    fn next_field(
        &self,
        schema: &'schema Valid<Schema>,
        field_name: &str,
    ) -> Result<FieldWithParent<'schema>, String> {
        let composite_type = self.next_parent_type(schema);
        let field_definition = composite_type.field(field_name).ok_or_else(|| {
            format!(
                "Field `{}` does not exist on type `{}`",
                field_name,
                composite_type.name()
            )
        })?;
        Ok(FieldWithParent {
            def: field_definition,
            parent: composite_type,
        })
    }
}

impl std::fmt::Debug for FieldPath<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            self.0.iter().map(|f| format!("{:?}", f)).join(" | ")
        )
    }
}

// --- Right -----------------------------------------------------------------------------------------------------------

#[derive(Clone)]
pub(crate) enum Expression<'sel> {
    Key(&'sel Key),
    KnownVariable(&'sel KnownVariable),
    LitExpr(&'sel LitExpr),
    Method(&'sel String, &'sel Option<MethodArgs>),
}

impl<'sel> std::fmt::Debug for Expression<'sel> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Expression::Key(key) => write!(f, "{}", key),
            Expression::KnownVariable(var) => write!(f, "{}", var),
            Expression::LitExpr(expr) => write!(
                f,
                "LitExpr({})",
                expr.pretty_print_with_indentation(true, 0)
            ),
            Expression::Method(name, args) => {
                write!(
                    f,
                    "->{}{}",
                    name,
                    &args
                        .as_ref()
                        .map(|a| format!("({})", a.pretty_print()))
                        .unwrap_or(String::new())
                )
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct ExpressionPath<'selection>(Vec<Expression<'selection>>);

impl<'selection> ExpressionPath<'selection> {
    fn add(&self, expression: Expression<'selection>) -> Self {
        let mut new = self.clone();
        new.0.push(expression);
        new
    }

    fn add_with_tail(
        &self,
        expr: Expression<'selection>,
        tail: Vec<Expression<'selection>>,
    ) -> Self {
        let mut new = self.clone();
        new.0.push(expr);
        new.0.extend(tail);
        new
    }

    fn add_tail(&self, tail: Vec<Expression<'selection>>) -> Self {
        let mut new = self.clone();
        new.0.extend(tail);
        new
    }
}

impl std::fmt::Debug for ExpressionPath<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            self.0.iter().map(|f| format!("{:?}", f)).join(" | ")
        )
    }
}

// ---------------------------------------------------------------------------------------------------------------------

impl<'sel> WithRange<PathList> {
    fn flatten_with_tail(&'sel self) -> (Vec<Expression<'sel>>, Option<&'sel SubSelection>) {
        match self.as_ref() {
            PathList::Var(var, tail) => {
                let mut expression_path = vec![Expression::KnownVariable(var)];
                let (tail, sub_selection) = tail.flatten_with_tail();
                expression_path.extend(tail);
                (expression_path, sub_selection)
            }
            PathList::Key(key, tail) => {
                let mut expression_path = vec![Expression::Key(key)];
                let (tail, sub_selection) = tail.flatten_with_tail();
                expression_path.extend(tail);
                (expression_path, sub_selection)
            }
            PathList::Expr(expr, tail) => {
                let mut expression_path = vec![Expression::LitExpr(expr)];
                let (tail, sub_selection) = tail.flatten_with_tail();
                expression_path.extend(tail);
                (expression_path, sub_selection)
            }
            PathList::Method(name, args, tail) => {
                let mut expression_path = vec![Expression::Method(name, args)];
                let (tail, sub_selection) = tail.flatten_with_tail();
                expression_path.extend(tail);
                (expression_path, sub_selection)
            }
            PathList::Selection(sub_selection) => (vec![], Some(sub_selection)),
            PathList::Empty => (vec![], None),
        }
    }
}

// ---- Public API -----------------------------------------------------------------------------------------------------

impl JSONSelection {
    #[allow(unused)]
    pub(crate) fn single_assignment<'schema, 'sel>(
        &'sel self,
        schema: &'schema Valid<Schema>,
        starting_from: (&'schema ExtendedType, &'schema Component<FieldDefinition>),
    ) -> (Vec<Assignment<'schema, 'sel>>, Vec<String>) {
        let mut errors: Vec<String> = vec![];
        let starting_from = FieldWithParent {
            def: starting_from.1,
            parent: starting_from.0.try_into().unwrap(),
        };
        let assignments = self.build_single_assignment(
            schema,
            FieldPath(vec![starting_from]),
            ExpressionPath(vec![]),
            &mut errors,
        );
        (assignments, errors)
    }
}

pub(super) trait SingleAssignmentInternal {
    fn build_single_assignment<'schema, 'sel>(
        &'sel self,
        schema: &'schema Valid<Schema>,
        field_path: FieldPath<'schema>,
        expression_path: ExpressionPath<'sel>,
        errors: &mut Vec<String>,
    ) -> Vec<Assignment<'schema, 'sel>>;
}

// --- JSONSelection ---------------------------------------------------------------------------------------------------

impl SingleAssignmentInternal for JSONSelection {
    fn build_single_assignment<'schema, 'sel>(
        &'sel self,
        schema: &'schema Valid<Schema>,
        field_path: FieldPath<'schema>,
        expression_path: ExpressionPath<'sel>,
        errors: &mut Vec<String>,
    ) -> Vec<Assignment<'schema, 'sel>> {
        match self {
            Self::Named(sub_selection) => {
                sub_selection.build_single_assignment(schema, field_path, expression_path, errors)
            }
            Self::Path(path_selection) => {
                path_selection.build_single_assignment(schema, field_path, expression_path, errors)
            }
        }
    }
}

// --- NamedSelection --------------------------------------------------------------------------------------------------

impl SingleAssignmentInternal for NamedSelection {
    #[tracing::instrument(skip_all, name = "NamedSelection")]
    fn build_single_assignment<'schema, 'sel>(
        &'sel self,
        schema: &'schema Valid<Schema>,
        field_path: FieldPath<'schema>,
        expression_path: ExpressionPath<'sel>,
        errors: &mut Vec<String>,
    ) -> Vec<Assignment<'schema, 'sel>> {
        tracing::info!("{field_path:?} = {}", self.pretty_print());
        match self {
            Self::Field(alias, key, selection) => {
                let field_name = alias
                    .as_ref()
                    .map(|a| a.name())
                    .unwrap_or_else(|| key.as_str());

                let next_field = match field_path.next_field(schema, field_name) {
                    Ok(f) => f,
                    Err(e) => {
                        errors.push(e);
                        return vec![];
                    }
                };

                let output_type = next_field.def.ty.inner_named_type();
                let output_type = schema.types.get(output_type).unwrap();

                let coord = format!("{:?}", &next_field);
                let field_path = field_path.add(next_field);
                let expression_path = expression_path.add(Expression::Key(key));

                match (selection, output_type.is_leaf()) {
                    (Some(_), true) => {
                        errors.push(format!(
                            "Assignment to leaf field `{coord}` must not have subselections",
                        ));
                        vec![]
                    }
                    (None, false) => {
                        errors.push(
                            "Assignment to composite field `{coord}` must have have subselections"
                                .into(),
                        );
                        vec![]
                    }
                    (None, true) => {
                        vec![Assignment {
                            left: field_path,
                            right: expression_path,
                        }]
                    }
                    (Some(selection), false) => selection.build_single_assignment(
                        schema,
                        field_path.clone(),
                        expression_path,
                        errors,
                    ),
                }
            }
            Self::Path(alias_opt, path_selection) => {
                if let Some(alias) = alias_opt {
                    let field_name = alias.name();

                    let next_field = match field_path.next_field(schema, field_name) {
                        Ok(f) => f,
                        Err(e) => {
                            errors.push(e);
                            return vec![];
                        }
                    };

                    // let coord = format!("{:?}", &next_field);
                    // let output_type = next_field.def.ty.inner_named_type();
                    // let output_type = schema.types.get(output_type).unwrap();

                    // if output_type.is_leaf() {
                    //     errors.push(format!(
                    //         "Assignment to leaf field `{coord}` must not have subselections",
                    //     ));
                    //     return vec![];
                    // }

                    let field_path = field_path.add(next_field);

                    path_selection.build_single_assignment(
                        schema,
                        field_path,
                        expression_path,
                        errors,
                    )
                } else {
                    path_selection.build_single_assignment(
                        schema,
                        field_path,
                        expression_path,
                        errors,
                    )
                }
            }
            Self::Group(alias, sub_selection) => {
                let field_name = alias.name();

                let next_field = match field_path.next_field(schema, field_name) {
                    Ok(f) => f,
                    Err(e) => {
                        errors.push(e);
                        return vec![];
                    }
                };

                // let coord = format!("{:?}", &next_field);
                // let output_type = next_field.def.ty.inner_named_type();
                // let output_type = schema.types.get(output_type).unwrap();

                // if output_type.is_leaf() {
                //     errors.push(format!(
                //         "Assignment to leaf field `{coord}` must not have subselections",
                //     ));
                //     return vec![];
                // }

                let field_path = field_path.add(next_field);

                sub_selection.build_single_assignment(schema, field_path, expression_path, errors)
            }
        }
    }
}

// --- SubSelection ----------------------------------------------------------------------------------------------------

impl SingleAssignmentInternal for SubSelection {
    #[tracing::instrument(skip_all, name = "SubSelection")]
    fn build_single_assignment<'schema, 'sel>(
        &'sel self,
        schema: &'schema Valid<Schema>,
        field_path: FieldPath<'schema>,
        expression_path: ExpressionPath<'sel>,
        errors: &mut Vec<String>,
    ) -> Vec<Assignment<'schema, 'sel>> {
        tracing::info!(
            "{field_path:?} = {}",
            self.pretty_print_with_indentation(true, 0)
        );
        self.selections
            .iter()
            .flat_map(|s| {
                s.build_single_assignment(
                    schema,
                    field_path.clone(),
                    expression_path.clone(),
                    errors,
                )
            })
            .collect()
    }
}

// --- PathSelection ---------------------------------------------------------------------------------------------------

impl SingleAssignmentInternal for PathSelection {
    #[tracing::instrument(skip_all, name = "PathSelection")]
    fn build_single_assignment<'schema, 'sel>(
        &'sel self,
        schema: &'schema Valid<Schema>,
        field_path: FieldPath<'schema>,
        expression_path: ExpressionPath<'sel>,
        errors: &mut Vec<String>,
    ) -> Vec<Assignment<'schema, 'sel>> {
        tracing::info!(
            "{field_path:?} = {}",
            self.pretty_print_with_indentation(true, 0)
        );
        self.path
            .build_single_assignment(schema, field_path, expression_path, errors)
    }
}

// --- PathList --------------------------------------------------------------------------------------------------------

impl SingleAssignmentInternal for WithRange<PathList> {
    #[tracing::instrument(skip_all, name = "PathList")]
    fn build_single_assignment<'schema, 'sel>(
        &'sel self,
        schema: &'schema Valid<Schema>,
        field_path: FieldPath<'schema>,
        expression_path: ExpressionPath<'sel>,
        errors: &mut Vec<String>,
    ) -> Vec<Assignment<'schema, 'sel>> {
        tracing::info!(
            "{field_path:?} = {}",
            self.pretty_print_with_indentation(true, 0)
        );

        match self.as_ref() {
            // var is always at the beginning of a PathList
            PathList::Var(var, tail) => {
                let (expressions, sub_selection) = tail.flatten_with_tail();
                let expression_path =
                    expression_path.add_with_tail(Expression::KnownVariable(var), expressions);

                if let Some(sub_selection) = sub_selection {
                    sub_selection.build_single_assignment(
                        schema,
                        field_path,
                        expression_path,
                        errors,
                    )
                } else {
                    vec![Assignment {
                        left: field_path,
                        right: expression_path,
                    }]
                }
            }

            // key can be at any position in the path list
            PathList::Key(key, tail) => {
                let (expressions, sub_selection) = tail.flatten_with_tail();
                let expression_path =
                    expression_path.add_with_tail(Expression::Key(key), expressions);

                if let Some(sub_selection) = sub_selection {
                    sub_selection.build_single_assignment(
                        schema,
                        field_path,
                        expression_path,
                        errors,
                    )
                } else {
                    vec![Assignment {
                        left: field_path,
                        right: expression_path,
                    }]
                }
            }

            // Literal expressions must be at the start of a PathList
            // If the tail is empty, we'll use the literal expression for assignment
            //      If the expression is an array, we'll recurse into it to see if we can use it for assignment
            //      If the expression is a scalar, we'll return an assignment here
            //      If the expression is an object, we'll recurse into it and create assignments for its fields
            // If the tail is not empty, the literal is input for keys, methods, or selections
            //      Append it to the expression path and recurse
            PathList::Expr(expr, tail) => {
                let (expressions, sub_selection) = tail.flatten_with_tail();
                let expression_path =
                    expression_path.add_with_tail(Expression::LitExpr(expr), expressions);

                if let Some(sub_selection) = sub_selection {
                    sub_selection.build_single_assignment(
                        schema,
                        field_path,
                        expression_path,
                        errors,
                    )
                } else {
                    expr.build_single_assignment(schema, field_path, expression_path, errors)
                }
            }

            // methods cannot be at the beginning of a PathList
            PathList::Method(_, _, _) => {
                debug_assert!(
                    false,
                    "PathList::Method should be handled with flatten_with_tail()"
                );
                vec![]
            }

            // selection must be at the end of a PathList
            PathList::Selection(_) => {
                debug_assert!(
                    false,
                    "PathList::Selection should be handled with flatten_with_tail()"
                );
                vec![]
            }

            // empty must be at the end of a PathList
            PathList::Empty => {
                debug_assert!(
                    false,
                    "PathList::Empty should be handled with flatten_with_tail()"
                );
                vec![]
            }
        }
    }
}

// --- LitExpr ---------------------------------------------------------------------------------------------------------

impl SingleAssignmentInternal for WithRange<LitExpr> {
    #[tracing::instrument(skip_all, name = "LitExpr")]
    fn build_single_assignment<'schema, 'sel>(
        &'sel self,
        schema: &'schema Valid<Schema>,
        field_path: FieldPath<'schema>,
        expression_path: ExpressionPath<'sel>,
        errors: &mut Vec<String>,
    ) -> Vec<Assignment<'schema, 'sel>> {
        tracing::info!(
            "{field_path:?} = {}",
            self.pretty_print_with_indentation(true, 0)
        );

        match self.as_ref() {
            LitExpr::String(_) | LitExpr::Number(_) | LitExpr::Bool(_) | LitExpr::Null => {
                vec![Assignment {
                    left: field_path,
                    right: expression_path.add(Expression::LitExpr(self)),
                }]
            }

            LitExpr::Array(arr) => {
                // TODO: what if the items are different types?!
                if let Some(item) = arr.first() {
                    item.build_single_assignment_abstract(
                        schema,
                        field_path,
                        expression_path,
                        errors,
                    )
                } else {
                    vec![Assignment {
                        left: field_path,
                        right: expression_path,
                    }]
                }
            }

            LitExpr::Object(index_map) => {
                index_map
                    .iter()
                    .flat_map(|(key, value)| {
                        let field_name = key.as_str();

                        let next_field = match field_path.next_field(schema, field_name) {
                            Ok(f) => f,
                            Err(e) => {
                                errors.push(e);
                                return vec![];
                            }
                        };

                        // let output_type = next_field.def.ty.inner_named_type();
                        // let output_type = schema.types.get(output_type).unwrap();

                        let field_path = field_path.add(next_field);
                        let expression_path = expression_path.add(Expression::Key(key));

                        value.build_single_assignment(
                            schema,
                            field_path.clone(),
                            expression_path,
                            errors,
                        )
                    })
                    .collect()
            }
            LitExpr::Path(path) => {
                let (expressions, sub_selection) = path.path.flatten_with_tail();
                let expression_path = expression_path.add_tail(expressions);

                if let Some(sub_selection) = sub_selection {
                    sub_selection.build_single_assignment(
                        schema,
                        field_path,
                        expression_path,
                        errors,
                    )
                } else {
                    vec![Assignment {
                        left: field_path,
                        right: expression_path,
                    }]
                }
            }
        }
    }
}

trait SingleAssignmentHelper {
    fn build_single_assignment_abstract<'schema, 'sel>(
        &'sel self,
        schema: &'schema Valid<Schema>,
        field_path: FieldPath<'schema>,
        expression_path: ExpressionPath<'sel>,
        errors: &mut Vec<String>,
    ) -> Vec<Assignment<'schema, 'sel>>;
}

/// This is a special case for when we encounter a LitExpr::Object inside another
/// literal (and array).
///
/// With an array, we look at the first item and use that to determine the shape
/// of the values in the array. (We don't current have a solution for polymorphic arrays).
///
/// Because we're using the first item as an example of the shape, we don't want
/// to actually use its literal values for assignment. Instead, we'll just terminate
/// with the relevant selections for this assignment. When we evaluate the expression
/// path, we'll apply the selection to each item in the array.
impl SingleAssignmentHelper for WithRange<LitExpr> {
    #[tracing::instrument(skip_all, name = "LitExpr(ObjectSpecialCase)")]
    fn build_single_assignment_abstract<'schema, 'sel>(
        &'sel self,
        schema: &'schema Valid<Schema>,
        field_path: FieldPath<'schema>,
        expression_path: ExpressionPath<'sel>,
        errors: &mut Vec<String>,
    ) -> Vec<Assignment<'schema, 'sel>> {
        match self.as_ref() {
            LitExpr::String(_) | LitExpr::Number(_) | LitExpr::Bool(_) | LitExpr::Null => {
                vec![Assignment {
                    left: field_path,
                    right: expression_path,
                }]
            }

            LitExpr::Array(_) | LitExpr::Path(_) => {
                self.build_single_assignment(schema, field_path, expression_path, errors)
            }

            LitExpr::Object(index_map) => index_map
                .iter()
                .flat_map(|(key, value)| {
                    let field_name = key.as_str();

                    let next_field = match field_path.next_field(schema, field_name) {
                        Ok(f) => f,
                        Err(e) => {
                            errors.push(e);
                            return vec![];
                        }
                    };

                    let field_path = field_path.add(next_field);
                    let expression_path = expression_path.add(Expression::Key(key));

                    match value.as_ref() {
                        LitExpr::String(_)
                        | LitExpr::Number(_)
                        | LitExpr::Bool(_)
                        | LitExpr::Null => {
                            vec![Assignment {
                                left: field_path,
                                right: expression_path,
                            }]
                        }
                        LitExpr::Array(_) | LitExpr::Path(_) => value.build_single_assignment(
                            schema,
                            field_path,
                            expression_path,
                            errors,
                        ),

                        LitExpr::Object(_) => value.build_single_assignment_abstract(
                            schema,
                            field_path,
                            expression_path,
                            errors,
                        ),
                    }
                })
                .collect(),
        }
    }
}

// --- TESTS -----------------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use insta::assert_debug_snapshot;

    use super::JSONSelection;

    #[test_log::test]
    fn test0() {
        let (_, s) = JSONSelection::parse("a b: c").unwrap();
        let schema = Schema::parse_and_validate(
            r#"
            type Query {
                f: T
            }

            type T {
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
        assert_eq!(errors, Vec::<String>::new());
        assert_debug_snapshot!(assignments, @r###"
        [
            Assignment {
                left: Query.f: T | T.a: Int,
                right: .a,
            },
            Assignment {
                left: Query.f: T | T.b: Int,
                right: .c,
            },
        ]
        "###);
    }

    #[test_log::test]
    fn test2() {
        let (_, s) = JSONSelection::parse("a { b c }").unwrap();
        let schema = Schema::parse_and_validate(
            r#"
            type Query {
                f: T
            }

            type T {
                a: A
            }

            type A {
                b: String
                c: String
            }
            "#,
            "",
        )
        .unwrap();
        let parent_type = schema.types.get("Query").unwrap();
        let connector_field = schema.type_field("Query", "f").unwrap();
        let (assignments, errors) = s.single_assignment(&schema, (parent_type, connector_field));
        assert_eq!(errors, Vec::<String>::new());
        assert_debug_snapshot!(assignments, @r###"
        [
            Assignment {
                left: Query.f: T | T.a: A | A.b: String,
                right: .a | .b,
            },
            Assignment {
                left: Query.f: T | T.a: A | A.c: String,
                right: .a | .c,
            },
        ]
        "###);
    }

    #[test_log::test]
    fn test3() {
        let (_, s) = JSONSelection::parse("$.a { b c }").unwrap();
        let schema = Schema::parse_and_validate(
            r#"
            type Query {
                f: T
            }

            type T {
                b: String
                c: String
            }
            "#,
            "",
        )
        .unwrap();
        let parent_type = schema.types.get("Query").unwrap();
        let connector_field = schema.type_field("Query", "f").unwrap();
        let (assignments, errors) = s.single_assignment(&schema, (parent_type, connector_field));
        assert_eq!(errors, Vec::<String>::new());
        assert_debug_snapshot!(assignments, @r###"
        [
            Assignment {
                left: Query.f: T | T.b: String,
                right: $ | .a | .b,
            },
            Assignment {
                left: Query.f: T | T.c: String,
                right: $ | .a | .c,
            },
        ]
        "###);
    }

    #[test_log::test]
    fn test4() {
        let (_, s) = JSONSelection::parse("$.a { b: $.c.d { e: f } }").unwrap();
        let schema = Schema::parse_and_validate(
            r#"
            type Query {
                f: T
            }

            type T {
                b: B
            }

            type B {
                e: String
            }
            "#,
            "",
        )
        .unwrap();
        let parent_type = schema.types.get("Query").unwrap();
        let connector_field = schema.type_field("Query", "f").unwrap();
        let (assignments, errors) = s.single_assignment(&schema, (parent_type, connector_field));
        assert_eq!(errors, Vec::<String>::new());
        assert_debug_snapshot!(assignments, @r###"
        [
            Assignment {
                left: Query.f: T | T.b: B | B.e: String,
                right: $ | .a | $ | .c | .d | .f,
            },
        ]
        "###);
    }

    #[test_log::test]
    fn test5() {
        let (_, s) = JSONSelection::parse("a: $({ b: 1, c: $.c, d: $.d { e } })").unwrap();
        let schema = Schema::parse_and_validate(
            r#"
            type Query {
                f: T
            }

            type T {
                a: A
            }

            type A {
                b: Int
                c: Int
                d: D
            }

            type D {
                e: Int
            }
            "#,
            "",
        )
        .unwrap();
        let parent_type = schema.types.get("Query").unwrap();
        let connector_field = schema.type_field("Query", "f").unwrap();
        let (assignments, errors) = s.single_assignment(&schema, (parent_type, connector_field));
        assert_eq!(errors, Vec::<String>::new());
        assert_debug_snapshot!(assignments, @r###"
        [
            Assignment {
                left: Query.f: T | T.a: A | A.b: Int,
                right: LitExpr({"b": 1, "c": $.c, "d": $.d {
                  e
                }}) | .b | LitExpr(1),
            },
            Assignment {
                left: Query.f: T | T.a: A | A.c: Int,
                right: LitExpr({"b": 1, "c": $.c, "d": $.d {
                  e
                }}) | .c | $ | .c,
            },
            Assignment {
                left: Query.f: T | T.a: A | A.d: D | D.e: Int,
                right: LitExpr({"b": 1, "c": $.c, "d": $.d {
                  e
                }}) | .d | $ | .d | .e,
            },
        ]
        "###);
    }

    #[test_log::test]
    fn test6() {
        let (_, s) = JSONSelection::parse(
            "a: a->echo(@) b: b->entries { key value } c: c->map({ id: @ })->first { id }",
        )
        .unwrap();
        let schema = Schema::parse_and_validate(
            r#"
            type Query {
                f: T
            }

            type T {
                a: String
                b: [KV]
                c: C
            }

            type KV {
                key: String
                value: Int
            }

            type C {
              id: ID
            }
            "#,
            "",
        )
        .unwrap();
        let parent_type = schema.types.get("Query").unwrap();
        let connector_field = schema.type_field("Query", "f").unwrap();
        let (assignments, errors) = s.single_assignment(&schema, (parent_type, connector_field));
        assert_eq!(errors, Vec::<String>::new());
        assert_debug_snapshot!(assignments, @r###"
        [
            Assignment {
                left: Query.f: T | T.a: String,
                right: .a | ->echo((@)),
            },
            Assignment {
                left: Query.f: T | T.b: [KV] | KV.key: String,
                right: .b | ->entries | .key,
            },
            Assignment {
                left: Query.f: T | T.b: [KV] | KV.value: Int,
                right: .b | ->entries | .value,
            },
            Assignment {
                left: Query.f: T | T.c: C | C.id: ID,
                right: .c | ->map(({"id": @})) | ->first | .id,
            },
        ]
        "###)
    }

    #[test_log::test]
    fn test7() {
        let (_, s) = JSONSelection::parse(
            "
              a: $([{ b: 1 }]) { b }
              c: $([1,2,3])
              d: $([{ e: 1 }, { e: 2 }])
              f: $([{ g: { h: 1 } }])
            ",
        )
        .unwrap();
        let schema = Schema::parse_and_validate(
            r#"
            type Query {
                f: T
            }

            type T {
                a: A
                c: [Int]
                d: [D]
                f: [F]
            }

            type A {
                b: String
            }

            type D {
              e: Int
            }

            type F {
              g: G
            }

            type G {
              h: Int
            }
            "#,
            "",
        )
        .unwrap();
        let parent_type = schema.types.get("Query").unwrap();
        let connector_field = schema.type_field("Query", "f").unwrap();
        let (assignments, errors) = s.single_assignment(&schema, (parent_type, connector_field));
        assert_eq!(errors, Vec::<String>::new());
        assert_debug_snapshot!(assignments, @r###"
        [
            Assignment {
                left: Query.f: T | T.a: A | A.b: String,
                right: LitExpr([{"b": 1}]) | .b,
            },
            Assignment {
                left: Query.f: T | T.c: [Int],
                right: LitExpr([1, 2, 3]),
            },
            Assignment {
                left: Query.f: T | T.d: [D] | D.e: Int,
                right: LitExpr([{"e": 1}, {"e": 2}]) | .e,
            },
            Assignment {
                left: Query.f: T | T.f: [F] | F.g: G | G.h: Int,
                right: LitExpr([{"g": {"h": 1}}]) | .g | .h,
            },
        ]
        "###)
    }

    #[test_log::test]
    fn test8() {
        let (_, s) = JSONSelection::parse("$.a.b { c d.e.f { g h } i: { j k } }").unwrap();
        let schema = Schema::parse_and_validate(
            r#"
            type Query {
                f: T
            }

            type T {
                c: Int
                g: Int
                h: Int
                i: I
            }

            type I {
              j: Int
              k: Int
            }
            "#,
            "",
        )
        .unwrap();
        let parent_type = schema.types.get("Query").unwrap();
        let connector_field = schema.type_field("Query", "f").unwrap();
        let (assignments, errors) = s.single_assignment(&schema, (parent_type, connector_field));
        assert_eq!(errors, Vec::<String>::new());
        assert_debug_snapshot!(assignments, @r###"
        [
            Assignment {
                left: Query.f: T | T.c: Int,
                right: $ | .a | .b | .c,
            },
            Assignment {
                left: Query.f: T | T.g: Int,
                right: $ | .a | .b | .d | .e | .f | .g,
            },
            Assignment {
                left: Query.f: T | T.h: Int,
                right: $ | .a | .b | .d | .e | .f | .h,
            },
            Assignment {
                left: Query.f: T | T.i: I | I.j: Int,
                right: $ | .a | .b | .j,
            },
            Assignment {
                left: Query.f: T | T.i: I | I.k: Int,
                right: $ | .a | .b | .k,
            },
        ]
        "###)
    }

    #[test_log::test]
    fn test9() {
        let (_, s) = JSONSelection::parse(
            r#"
        a: $->echo(@)
        b: c->entries { key value }
        d: e->match([1, "one"],[@, "other"])->first"#,
        )
        .unwrap();
        let schema = Schema::parse_and_validate(
            r#"
            type Query {
                f: T
            }

            type T {
                a: String
                b: [KV]
                d: String
            }

            type KV {
                key: String
                value: Int
            }
            "#,
            "",
        )
        .unwrap();
        let parent_type = schema.types.get("Query").unwrap();
        let connector_field = schema.type_field("Query", "f").unwrap();
        let (assignments, errors) = s.single_assignment(&schema, (parent_type, connector_field));
        assert_eq!(errors, Vec::<String>::new());
        assert_debug_snapshot!(assignments, @r###"
        [
            Assignment {
                left: Query.f: T | T.a: String,
                right: $ | ->echo((@)),
            },
            Assignment {
                left: Query.f: T | T.b: [KV] | KV.key: String,
                right: .c | ->entries | .key,
            },
            Assignment {
                left: Query.f: T | T.b: [KV] | KV.value: Int,
                right: .c | ->entries | .value,
            },
            Assignment {
                left: Query.f: T | T.d: String,
                right: .e | ->match(([1, "one"], [@, "other"])) | ->first,
            },
        ]
        "###)
    }
}
