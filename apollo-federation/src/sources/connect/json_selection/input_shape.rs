use std::fmt;
use std::fmt::Display;
use std::fmt::Formatter;
use std::ops::Range;
use std::vec;

use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use itertools::Itertools;
use shape::graphql::shapes_for_schema;
use shape::Shape;

use super::known_var::KnownVariable;
use super::lit_expr::LitExpr;
use super::location::merge_ranges;
use super::location::WithRange;
use super::JSONSelection;
use super::MethodArgs;
use super::NamedSelection;
use super::PathList;
use super::PathSelection;
use super::Ranged;
use super::SubSelection;

#[derive(Debug, Default)]
pub struct UnresolvedShape(IndexMap<String, UnresolvedShape>, Vec<Range<usize>>);

impl UnresolvedShape {
    fn insert(&mut self, path: &[&str], range: Option<Range<usize>>) {
        let mut node = self;
        for head in path {
            node = node.0.entry(head.to_string()).or_default();
        }
        if let Some(range) = range {
            node.1.push(range);
        }
    }
}

impl Display for UnresolvedShape {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for (i, (key, node)) in self.0.iter().enumerate() {
            write!(f, "{}", key)?;
            if !node.1.is_empty() {
                let ranges = node.1.iter().map(|r| format!("{:?}", r)).join(", ");
                write!(f, "({})", ranges)?;
            }
            if !node.0.is_empty() {
                write!(f, " {{ {} }}", node)?;
            }
            if i != self.0.len() - 1 {
                write!(f, " ")?;
            }
        }
        Ok(())
    }
}

trait InputShape {
    fn compute_input_shape(&self, trie: &mut UnresolvedShape, context: &[&str]);
}

impl JSONSelection {
    pub fn input_shape(&self) -> UnresolvedShape {
        let mut trie = UnresolvedShape::default();
        self.compute_input_shape(&mut trie, &[]);
        trie
    }
}

impl InputShape for JSONSelection {
    fn compute_input_shape(&self, trie: &mut UnresolvedShape, context: &[&str]) {
        match self {
            JSONSelection::Named(sub_selection) => sub_selection.compute_input_shape(trie, context),
            JSONSelection::Path(path_selection) => {
                path_selection.compute_input_shape(trie, context)
            }
        }
    }
}

impl InputShape for SubSelection {
    fn compute_input_shape(&self, trie: &mut UnresolvedShape, context: &[&str]) {
        self.selections
            .iter()
            .for_each(|selection| selection.compute_input_shape(trie, context));
    }
}

impl InputShape for PathSelection {
    fn compute_input_shape(&self, trie: &mut UnresolvedShape, context: &[&str]) {
        self.path.compute_input_shape(trie, context)
    }
}

impl InputShape for NamedSelection {
    fn compute_input_shape(&self, trie: &mut UnresolvedShape, context: &[&str]) {
        match self {
            NamedSelection::Field(_, key, sub_selection) => {
                let path = if context.is_empty() {
                    vec!["$", key.as_str()]
                } else {
                    let mut path = context.to_vec();
                    path.push(key.as_str());
                    path
                };
                trie.insert(&path, None);
                sub_selection
                    .as_ref()
                    .map(|s| s.compute_input_shape(trie, path.as_slice()));
            }
            NamedSelection::Path {
                alias: _,
                inline: _,
                path,
            } => {
                path.compute_input_shape(trie, context);
            }
            NamedSelection::Group(_, sub_selection) => {
                sub_selection.compute_input_shape(trie, context)
            }
        }
    }
}

impl InputShape for PathList {
    fn compute_input_shape(&self, trie: &mut UnresolvedShape, context: &[&str]) {
        match self {
            PathList::Var(var, _) => {
                let (path, methods, sub) = unwind(self);
                let str_path = path.iter().map(|p| *p.as_ref()).collect::<Vec<_>>();
                let last_range = path.last().and_then(|p| p.range());
                let full_range = merge_ranges(var.range(), last_range);

                // @ is something we'll deal with at output shape time
                if *var == KnownVariable::AtSign {
                    sub.as_ref().map(|s| s.compute_input_shape(trie, context));
                    methods
                        .iter()
                        .for_each(|m| m.compute_input_shape(trie, context));
                    return;
                }

                // if we're at the root, we need to include the $
                if *var == KnownVariable::Dollar && context.is_empty() {
                    trie.insert(&str_path, full_range);
                    sub.as_ref().map(|s| s.compute_input_shape(trie, &str_path));
                    methods
                        .iter()
                        .for_each(|m| m.compute_input_shape(trie, context));
                    return;
                }

                // if we're not at the root we're chaining onto the context
                if *var == KnownVariable::Dollar {
                    let mut all = context.to_vec();
                    all.extend(&str_path[1..]);
                    trie.insert(&all, full_range);
                    sub.as_ref().map(|s| s.compute_input_shape(trie, &all));
                    methods
                        .iter()
                        .for_each(|m| m.compute_input_shape(trie, context));
                    return;
                }

                // any other var and we're starting a new context
                trie.insert(&str_path, full_range);
                sub.as_ref().map(|s| s.compute_input_shape(trie, &str_path));
                methods
                    .iter()
                    .for_each(|m| m.compute_input_shape(trie, context));
            }
            PathList::Key(_, _) => {
                let (path, methods, sub) = unwind(self);
                let str_path = path.iter().map(|p| *p.as_ref()).collect::<Vec<_>>();
                let mut new_context = if context.is_empty() {
                    vec!["$"]
                } else {
                    context.to_vec()
                };
                new_context.extend(str_path);
                trie.insert(&new_context, None);
                sub.as_ref()
                    .map(|s| s.compute_input_shape(trie, &new_context));
                methods
                    .iter()
                    .for_each(|m| m.compute_input_shape(trie, context));
            }
            PathList::Expr(expr, path_list) => {
                expr.compute_input_shape(trie, context);
                let (_, methods, sub) = unwind(path_list);
                sub.as_ref().map(|s| s.compute_input_shape(trie, context));
                methods
                    .iter()
                    .for_each(|m| m.compute_input_shape(trie, context));
            }
            PathList::Method(_, method_args, path_list) => {
                method_args.as_ref().map(|m| {
                    m.args
                        .iter()
                        .for_each(|arg| arg.compute_input_shape(trie, context))
                });
                let (_, methods, sub) = unwind(path_list);
                sub.as_ref().map(|s| s.compute_input_shape(trie, context));
                methods
                    .iter()
                    .for_each(|m| m.compute_input_shape(trie, context));
            }
            PathList::Selection(sub_selection) => sub_selection.compute_input_shape(trie, context),
            PathList::Empty => {}
        }
    }
}

type Method<'a> = (&'a WithRange<String>, &'a Option<MethodArgs>);

impl<'a> InputShape for Method<'a> {
    fn compute_input_shape(&self, trie: &mut UnresolvedShape, context: &[&str]) {
        self.1.as_ref().map(|args| {
            args.args
                .iter()
                .for_each(|arg| arg.compute_input_shape(trie, context));
        });
    }
}

type StrRange<'a> = WithRange<&'a str>;

fn unwind<'a>(
    path_list: &'a PathList,
) -> (Vec<StrRange<'a>>, Vec<Method<'a>>, Option<&'a SubSelection>) {
    let mut head = Some(path_list);
    let mut keys = vec![];
    let mut methods = vec![];
    let mut sub = None;
    let mut past_keys = false;
    while let Some(next) = head {
        match next {
            PathList::Var(var, tail) => {
                keys.push(WithRange::new(var.as_str(), var.range()));
                head = Some(tail)
            }
            PathList::Key(key, tail) => {
                if !past_keys {
                    keys.push(WithRange::new(key.as_str(), key.range()));
                }
                head = Some(tail)
            }
            PathList::Expr(_, tail) => {
                past_keys = true;
                head = Some(tail)
            }
            PathList::Method(method, args, tail) => {
                past_keys = true;
                methods.push((method, args));
                head = Some(tail)
            }
            PathList::Selection(sub_selection) => {
                sub = Some(sub_selection);
                break;
            }
            PathList::Empty => break,
        }
    }

    (keys, methods, sub)
}

impl InputShape for LitExpr {
    fn compute_input_shape(&self, trie: &mut UnresolvedShape, context: &[&str]) {
        match self {
            LitExpr::String(_) | LitExpr::Number(_) | LitExpr::Bool(_) | LitExpr::Null => {}
            LitExpr::Object(index_map) => {
                index_map.iter().for_each(|(_, expr)| {
                    expr.compute_input_shape(trie, context);
                });
            }
            LitExpr::Array(vec) => {
                vec.iter().for_each(|expr| {
                    expr.compute_input_shape(trie, context);
                });
            }
            LitExpr::Path(path_selection) => path_selection.compute_input_shape(trie, context),
        }
    }
}

#[cfg(test)]
mod tests {
    use insta::assert_debug_snapshot;

    use super::JSONSelection;

    #[test]
    fn test_all_variables() {
        let s = JSONSelection::parse(
            "
            a: {
              b: {
                c: $args.c
              }
            }
            d: $args.d.e
            f: $args.f->echo($this.g)
            h: $({ i: $this.i.j })
            k: $.k
            ",
        )
        .unwrap();
        let input_shape = s.input_shape().to_string();
        assert_eq!(
            input_shape,
            "$args { c(56..63) d { e(109..118) } f(134..141) } $this { g(148..155) i { j(179..188) } } $ { k(207..210) }"
        );
    }

    #[test]
    fn test_implicit_root() {
        let s = JSONSelection::parse(
            "
            a
            b: c.d
            e: f->echo($.g)
            ",
        )
        .unwrap();
        let input_shape = s.input_shape().to_string();
        assert_eq!(input_shape, "$ { a c { d } f g(57..60) }");
    }

    #[test]
    fn test_contextual_vars() {
        let s = JSONSelection::parse(
            "
            $.a {
              $.b {
                c: $.c
                d: $.d->echo($.e)
              }
            }
            ",
        )
        .unwrap();

        let input_shape = s.input_shape().to_string();
        assert_eq!(
            input_shape,
            "$ { a(13..16) { b(33..36) { c(58..61) d(81..84) e(91..94) } } }"
        );
    }

    #[test]
    fn text_weird_shit() {
        let s = JSONSelection::parse(
            "
            a: $($.b).c.d->echo($.x)
            e: $->f(@.g).h.i->echo($.y->echo($.z, $.x))
            ",
        )
        .unwrap();

        let input_shape = s.input_shape().to_string();
        assert_eq!(
            input_shape,
            "$(53..54) { b(18..21) x(33..36, 88..91) y(73..76) z(83..86) }"
        );
    }

    #[test]
    fn shapes() {
        let schema = apollo_compiler::Schema::parse_and_validate(
            "
        type Query {
            f(a: Int, b: BInput, e: Float!, f: BInput!): T
        }

        type T {
            d: Boolean
        }

        input BInput {
            c: String
            c2: String!
        }
        ",
            "path",
        )
        .unwrap();
        let field_shapes =
            super::shapes_for_field(&schema, schema.type_field("Query", "f").unwrap(), None);

        let input_shape = JSONSelection::parse("$args { a b { c } e f { c2 } }")
            .unwrap()
            .input_shape();

        assert_debug_snapshot!(
            super::reconcile_input_shapes(&input_shape, &field_shapes),
            @r###"
        {
            "$args": { a: One<Int, null>, b: One<{ c: One<String, null> }, null>, e: Float, f: { c2: String } },
        }
        "###
        );
    }

    #[test]
    fn shapes_this() {
        let schema = apollo_compiler::Schema::parse_and_validate(
            "
        type Query {
            f(a: Int): T
        }

        type T {
            b: B
            d: Boolean!
            e(f: Int): String
        }

        type B {
            c: String!
        }
        ",
            "path",
        )
        .unwrap();

        let field_shapes = super::shapes_for_field(
            &schema,
            schema.type_field("T", "e").unwrap(),
            schema.types.get("T").and_then(|et| match et {
                apollo_compiler::schema::ExtendedType::Object(node) => Some(node),
                _ => None,
            }),
        );

        let input_shape = JSONSelection::parse("$args { f } $this { b { c } d }")
            .unwrap()
            .input_shape();

        assert_debug_snapshot!(
            super::reconcile_input_shapes(&input_shape, &field_shapes),
            @r###"
        {
            "$args": { f: One<Int, null> },
            "$this": { b: One<{ c: String }, null>, d: Bool },
        }
        "###
        );
    }

    #[test]
    fn shapes_errors() {
        let schema = apollo_compiler::Schema::parse_and_validate(
            "
        type Query {
            f(a: Int, b: BInput): T
        }

        type T {
            f: String
        }

        input BInput {
            c: String!
        }
        ",
            "path",
        )
        .unwrap();

        let field_shapes =
            super::shapes_for_field(&schema, schema.type_field("Query", "f").unwrap(), None);

        let input_shape = JSONSelection::parse("$args { a { x } b c }")
            .unwrap()
            .input_shape();

        assert_debug_snapshot!(
            super::reconcile_input_shapes(&input_shape, &field_shapes),
            @r###"
        {
            "$args": { a: One<Error<"`TODO` does not have a field named `TODO`">, null>, b: One<Error<"`TODO` is an object, so TODO must select fields within the object with `TODO`{}", { c: String }>, null>, c: Error<"`TODO` does not have a field named `c`"> },
        }
        "###
        );
    }
}

#[allow(unused)]
fn reconcile_input_shapes(
    trie: &UnresolvedShape,
    field_shapes: &IndexMap<&str, Shape>,
) -> IndexMap<String, Shape> {
    fn reconcile(
        field_shapes: &IndexMap<&str, Shape>,
        trie: &UnresolvedShape,
        shape: &Shape,
    ) -> Shape {
        match shape.case() {
            shape::ShapeCase::Name(name, _) => {
                if let Some(shape) = field_shapes.get(name.as_str()) {
                    reconcile(field_shapes, trie, shape)
                } else {
                    Shape::error_with_range(
                        format!("`{}` does not exist", name),
                        trie.1.first().cloned(),
                    )
                }
            }
            shape::ShapeCase::Object { fields, rest: _ } => {
                if trie.0.is_empty() {
                    return Shape::error_with_range_and_partial(
                        format!("`{}` is an object, so {} must select fields within the object with `{}`{{}}", "TODO", "TODO", "TODO"),
                        trie.1.first().cloned(),
                        shape.clone()
                    );
                }

                let mut new_fields = IndexMap::default();
                for (key, node) in &trie.0 {
                    if let Some(field) = fields.get(key) {
                        let value = reconcile(field_shapes, node, field);
                        new_fields.insert(key.to_string(), value);
                    } else {
                        new_fields.insert(
                            key.to_string(),
                            Shape::error_with_range(
                                format!("`{}` does not have a field named `{}`", "TODO", key),
                                trie.1.first().cloned(),
                            ),
                        );
                    }
                }
                Shape::record(indexmap::IndexMap::from_iter(new_fields))
            }
            shape::ShapeCase::All(shapes) => Shape::all(
                shapes
                    .iter()
                    .map(|s| reconcile(field_shapes, trie, s))
                    .collect::<IndexSet<_>>(),
            ),
            shape::ShapeCase::One(shapes) => Shape::one(
                shapes
                    .iter()
                    .map(|s| reconcile(field_shapes, trie, s))
                    .collect::<IndexSet<_>>(),
            ),

            shape::ShapeCase::Array { prefix: _, tail: _ } => todo!(),
            shape::ShapeCase::Unknown
            | shape::ShapeCase::None
            | shape::ShapeCase::Null
            | shape::ShapeCase::Error { .. } => shape.clone(),
            _ => {
                if trie.0.is_empty() {
                    shape.clone()
                } else {
                    Shape::error_with_range(
                        format!("`{}` does not have a field named `{}`", "TODO", "TODO"),
                        trie.1.first().cloned(),
                    )
                }
            }
        }
    }

    let mut new_fields = IndexMap::default();

    for (key, node) in &trie.0 {
        let field = field_shapes.get(key.as_str()).unwrap();
        let value = reconcile(field_shapes, node, field);
        new_fields.insert(key.to_string(), value);
    }

    new_fields
}

#[allow(unused)]
fn shapes_for_field<'s>(
    schema: &'s Schema,
    field_definition: &'s FieldDefinition,
    parent_type: Option<&'s Node<ObjectType>>,
) -> IndexMap<&'s str, Shape> {
    let args = shape_from_arguments(&field_definition.arguments);
    let mut shapes = IndexMap::from_iter([("$args", args)]);

    if let Some(parent_type) = parent_type {
        shapes.insert("$this", Shape::from(parent_type.as_ref()));
    }

    shapes.extend(shapes_for_schema(schema));
    shapes
}

fn shape_from_arguments(arguments: &Vec<Node<InputValueDefinition>>) -> Shape {
    let args = arguments
        .iter()
        .map(|arg| (arg.name.to_string(), Shape::from(arg.ty.as_ref())))
        .collect();
    Shape::record(args)
}
