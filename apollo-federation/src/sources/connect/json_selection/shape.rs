use apollo_compiler::collections::IndexMap;
use shape::Shape;
use shape::ShapeCase;

use super::known_var::KnownVariable;
use super::lit_expr::LitExpr;
use super::location::WithRange;
use super::methods::ArrowMethod;
use super::JSONSelection;
use super::NamedSelection;
use super::PathList;
use super::PathSelection;
use super::Ranged;
use super::SubSelection;

impl JSONSelection {
    /// Returns a [`ShapedSelection`] wrapping the JSONSelection, the currently
    /// known input shapes, and the computed output shape. This is typically
    /// only a starting point, meaning you will probably need to refine this
    /// [`ShapedSelection`] with [`ShapedSelection::refine`] or
    /// [`ShapedSelection::replace`] later.
    pub(crate) fn shaped_selection(&self) -> ShapedSelection {
        ShapedSelection::new(self.clone())
    }

    /// A quick way to get the most generic possible [`Shape`] for this
    /// [`JSONSelection`], without any additional named shapes specified.
    #[allow(dead_code)]
    pub(crate) fn shape(&self) -> Shape {
        self.output_shape(&IndexMap::default())
    }

    /// Called internally by [`ShapedSelection::compute`] to do the actual shape
    /// processing work. The root JSON input shape can be specified by defining
    /// the `$root` key in the `named_shapes` map.
    pub(crate) fn output_shape(&self, named_shapes: &IndexMap<String, Shape>) -> Shape {
        let input_shape = if let Some(root_shape) = named_shapes.get("$root") {
            root_shape.clone()
        } else {
            Shape::name("$root")
        };

        // At this level, $ and @ have the same value and shape.
        let dollar_shape = input_shape.clone();

        match self {
            Self::Named(selection) => {
                selection.compute_output_shape(input_shape, dollar_shape, named_shapes)
            }
            Self::Path(path_selection) => {
                path_selection.compute_output_shape(input_shape, dollar_shape, named_shapes)
            }
        }
    }
}

pub(crate) trait ComputeOutputShape {
    /// Computes the static output shape produced by a JSONSelection, by
    /// traversing the selection AST, recursively calling `compute_output_shape`
    /// on the current data/variable shapes at each level.
    fn compute_output_shape(
        &self,
        // Shape of the `@` variable, which typically changes with each
        // recursive call to compute_output_shape.
        input_shape: Shape,
        // Shape of the `$` variable, which is bound to the closest enclosing
        // subselection object, or the root data object if there is no enclosing
        // subselection.
        dollar_shape: Shape,
        // Shapes of other named variables, with the variable name `String`
        // including the initial `$` character. This map typically does not
        // change during the compute_output_shape recursion, and so can be
        // passed down by immutable reference.
        named_shapes: &IndexMap<String, Shape>,
    ) -> Shape;
}

impl<T: ComputeOutputShape> ComputeOutputShape for WithRange<T> {
    fn compute_output_shape(
        &self,
        input_shape: Shape,
        dollar_shape: Shape,
        named_shapes: &IndexMap<String, Shape>,
    ) -> Shape {
        self.as_ref()
            .compute_output_shape(input_shape, dollar_shape, named_shapes)
    }
}

impl ComputeOutputShape for JSONSelection {
    fn compute_output_shape(
        &self,
        input_shape: Shape,
        dollar_shape: Shape,
        named_shapes: &IndexMap<String, Shape>,
    ) -> Shape {
        match self {
            Self::Named(selection) => {
                selection.compute_output_shape(input_shape, dollar_shape, named_shapes)
            }
            Self::Path(path_selection) => {
                path_selection.compute_output_shape(input_shape, dollar_shape, named_shapes)
            }
        }
    }
}

impl ComputeOutputShape for NamedSelection {
    fn compute_output_shape(
        &self,
        input_shape: Shape,
        dollar_shape: Shape,
        named_shapes: &IndexMap<String, Shape>,
    ) -> Shape {
        let mut output = Shape::empty_map();

        match self {
            Self::Field(alias_opt, key, selection) => {
                let output_key = alias_opt
                    .as_ref()
                    .map_or(key.as_str(), |alias| alias.name());
                let field_shape = dollar_shape.field(key.as_str());
                output.insert(
                    output_key.to_string(),
                    if let Some(selection) = selection {
                        selection.compute_output_shape(field_shape, dollar_shape, named_shapes)
                    } else {
                        field_shape
                    },
                );
            }
            Self::Path { alias, path, .. } => {
                let path_shape = path.compute_output_shape(input_shape, dollar_shape, named_shapes);
                if let Some(alias) = alias {
                    output.insert(alias.name().to_string(), path_shape);
                } else {
                    return path_shape;
                }
            }
            Self::Group(alias, sub_selection) => {
                output.insert(
                    alias.name().to_string(),
                    sub_selection.compute_output_shape(input_shape, dollar_shape, named_shapes),
                );
            }
        };

        Shape::object(output, Shape::none())
    }
}

impl ComputeOutputShape for PathSelection {
    fn compute_output_shape(
        &self,
        input_shape: Shape,
        dollar_shape: Shape,
        named_shapes: &IndexMap<String, Shape>,
    ) -> Shape {
        match self.path.as_ref() {
            PathList::Key(_, _) => {
                // If this is a KeyPath, we need to evaluate the path starting
                // from the current $ shape, so we pass dollar_shape as the data
                // *and* dollar_shape to self.path.compute_output_shape.
                self.path.compute_output_shape(
                    dollar_shape.clone(),
                    dollar_shape.clone(),
                    named_shapes,
                )
            }
            // If this is not a KeyPath, keep evaluating against input_shape.
            // This logic parallels PathSelection::apply_to_path (above).
            _ => self
                .path
                .compute_output_shape(input_shape, dollar_shape, named_shapes),
        }
    }
}

impl ComputeOutputShape for PathList {
    fn compute_output_shape(
        &self,
        input_shape: Shape,
        dollar_shape: Shape,
        named_shapes: &IndexMap<String, Shape>,
    ) -> Shape {
        let current_shape = match self {
            Self::Var(ranged_var_name, _) => {
                let var_name = ranged_var_name.as_ref();
                if var_name == &KnownVariable::AtSign {
                    input_shape
                } else if var_name == &KnownVariable::Dollar {
                    dollar_shape.clone()
                } else if let Some(shape) = named_shapes.get(var_name.as_str()) {
                    shape.clone()
                } else {
                    Shape::name(var_name.as_str())
                }
            }

            // For the first key in a path, PathSelection::compute_output_shape
            // will have set our input_shape equal to its dollar_shape, thereby
            // ensuring that some.nested.path is equivalent to
            // $.some.nested.path.
            Self::Key(key, _) => input_shape.field(key.as_str()),

            Self::Expr(expr, _) => {
                expr.compute_output_shape(input_shape, dollar_shape.clone(), named_shapes)
            }

            Self::Method(method_name, method_args, _) => {
                if let Some(method) = ArrowMethod::lookup(method_name) {
                    method.shape(
                        method_name,
                        method_args.as_ref(),
                        input_shape,
                        dollar_shape.clone(),
                        named_shapes,
                    )
                } else {
                    let message = format!("Method ->{} not found", method_name.as_str());
                    return Shape::error_with_range(message.as_str(), method_name.range());
                }
            }

            Self::Selection(selection) => {
                selection.compute_output_shape(input_shape, dollar_shape.clone(), named_shapes)
            }

            Self::Empty => input_shape,
        };

        compute_tail_shape(self, current_shape, dollar_shape.clone(), named_shapes)
    }
}

fn compute_tail_shape(
    path: &PathList,
    input_shape: Shape,
    dollar_shape: Shape,
    named_shapes: &IndexMap<String, Shape>,
) -> Shape {
    match input_shape.case() {
        ShapeCase::None => input_shape,
        ShapeCase::One(shapes) => Shape::one(shapes.iter().map(|shape| {
            compute_tail_shape(path, shape.clone(), dollar_shape.clone(), named_shapes)
        })),
        ShapeCase::All(shapes) => Shape::all(shapes.iter().map(|shape| {
            compute_tail_shape(path, shape.clone(), dollar_shape.clone(), named_shapes)
        })),
        ShapeCase::Error(error) => ShapeCase::Error(shape::Error {
            message: error.message.clone(),
            range: error.range.clone(),
            partial: error.partial.as_ref().map(|shape| {
                compute_tail_shape(path, shape.clone(), dollar_shape.clone(), named_shapes)
            }),
        })
        .simplify(),
        _ => match path {
            PathList::Var(_, tail)
            | PathList::Key(_, tail)
            | PathList::Expr(_, tail)
            | PathList::Method(_, _, tail) => match input_shape.case() {
                ShapeCase::None => {
                    if tail.is_empty() {
                        input_shape
                    } else {
                        Shape::error_with_range("Path applied to nothing", tail.range())
                    }
                }
                _ => tail.compute_output_shape(input_shape, dollar_shape, named_shapes),
            },
            PathList::Selection(_) => input_shape,
            PathList::Empty => input_shape,
        },
    }
}

impl ComputeOutputShape for LitExpr {
    fn compute_output_shape(
        &self,
        input_shape: Shape,
        dollar_shape: Shape,
        named_shapes: &IndexMap<String, Shape>,
    ) -> Shape {
        match self {
            Self::Null => Shape::null(),
            Self::Bool(value) => Shape::bool_value(*value),
            Self::String(value) => Shape::string_value(value.as_str()),

            Self::Number(value) => {
                if let Some(n) = value.as_i64() {
                    Shape::int_value(n)
                } else if value.is_f64() {
                    Shape::float()
                } else {
                    Shape::error("Number neither Int nor Float")
                }
            }

            Self::Object(map) => {
                let mut fields = Shape::empty_map();
                for (key, value) in map {
                    fields.insert(
                        key.as_string(),
                        value.compute_output_shape(
                            input_shape.clone(),
                            dollar_shape.clone(),
                            named_shapes,
                        ),
                    );
                }
                Shape::object(fields, Shape::none())
            }

            Self::Array(vec) => {
                let mut shapes = Vec::with_capacity(vec.len());
                for value in vec {
                    shapes.push(value.compute_output_shape(
                        input_shape.clone(),
                        dollar_shape.clone(),
                        named_shapes,
                    ));
                }
                Shape::array(shapes, Shape::none())
            }

            Self::Path(path) => path.compute_output_shape(input_shape, dollar_shape, named_shapes),
        }
    }
}

impl ComputeOutputShape for SubSelection {
    fn compute_output_shape(
        &self,
        input_shape: Shape,
        dollar_shape: Shape,
        named_shapes: &IndexMap<String, Shape>,
    ) -> Shape {
        match input_shape.case() {
            ShapeCase::None => {
                return input_shape;
            }
            ShapeCase::One(cases) => {
                return Shape::one(cases.iter().map(|case| {
                    if case.is_none() {
                        case.clone()
                    } else {
                        self.compute_output_shape(case.clone(), dollar_shape.clone(), named_shapes)
                    }
                }));
            }
            ShapeCase::Array { prefix, tail } => {
                let new_prefix = prefix.iter().map(|shape| {
                    self.compute_output_shape(shape.clone(), dollar_shape.clone(), named_shapes)
                });

                let new_tail = if tail.is_none() {
                    tail.clone()
                } else {
                    self.compute_output_shape(tail.clone(), dollar_shape.clone(), named_shapes)
                };

                return Shape::array(new_prefix, new_tail);
            }
            _ => {}
        };

        // If input_shape is a named shape, it might end up being an array, so
        // for accuracy we apply the .* wildcard to indicate mapping over
        // possible array elements.
        let input_shape = input_shape.any_item();

        // The SubSelection rebinds the $ variable to the selected input object,
        // so we can ignore the previously received dollar_shape.
        let dollar_shape = input_shape.clone();

        // Build up the merged object shape using Shape::all to merge the
        // individual named_selection object shapes.
        let mut all_shape = Shape::empty_object();

        for named_selection in self.selections.iter() {
            // Simplifying as we go with Shape::all keeps all_shape relatively
            // small in the common case when all named_selection items return an
            // object shape, since those object shapes can all be merged
            // together into one object.
            all_shape = Shape::all([
                all_shape,
                named_selection.compute_output_shape(
                    input_shape.clone(),
                    dollar_shape.clone(),
                    named_shapes,
                ),
            ]);

            // If any named_selection item returns null instead of an object,
            // that nullifies the whole object and allows shape computation to
            // bail out early.
            if all_shape.is_null() {
                break;
            }
        }

        all_shape
    }
}

type Ref<T> = std::sync::Arc<T>;

#[derive(Debug, Clone)]
pub(crate) struct ShapedSelection {
    selection: Ref<JSONSelection>,
    named_shapes: IndexMap<String, Shape>,
    output_shape: Shape,
}

impl ShapedSelection {
    /// Takes ownership of the given [`JSONSelection`] and computes the output
    /// shape without any named shapes.
    pub(crate) fn new(selection: JSONSelection) -> Self {
        Self::compute(Ref::new(selection), IndexMap::default())
    }

    #[allow(dead_code)]
    pub(crate) fn selection(&self) -> &JSONSelection {
        self.selection.as_ref()
    }

    pub(crate) fn output_shape(&self) -> &Shape {
        &self.output_shape
    }

    /// Add new named shapes to this [`ShapedSelection`], using [`Shape::all`]
    /// to combine shapes wherever there are collisions with existing shapes.
    #[allow(dead_code)]
    pub(crate) fn refine(&self, new_named_shapes: IndexMap<String, Shape>) -> Self {
        let mut named_shapes = self.named_shapes.clone();
        for (new_name, new_shape) in new_named_shapes {
            if let Some(old_shape) = named_shapes.get_mut(&new_name) {
                *old_shape = Shape::all([old_shape.clone(), new_shape]);
            } else {
                named_shapes.insert(new_name, new_shape);
            }
        }
        Self::compute(self.selection.clone(), named_shapes)
    }

    /// Add new named shapes to this [`ShapedSelection`], replacing any existing
    /// shapes with the same name.
    #[allow(dead_code)]
    pub(crate) fn replace(&self, new_named_shapes: IndexMap<String, Shape>) -> Self {
        let mut named_shapes = self.named_shapes.clone();
        named_shapes.extend(new_named_shapes);
        Self::compute(self.selection.clone(), named_shapes)
    }

    fn compute(selection: Ref<JSONSelection>, named_shapes: IndexMap<String, Shape>) -> Self {
        let output_shape = selection.output_shape(&named_shapes);
        Self {
            selection,
            named_shapes,
            output_shape,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn pretty_print(&self) -> String {
        self.output_shape.pretty_print()
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::collections::IndexMap;
    use shape::Shape;

    use crate::selection;

    #[test]
    fn test_shaped_selection() {
        let selection = selection!("id name");
        let shaped_selection = selection.shaped_selection();
        assert_eq!(
            shaped_selection.pretty_print(),
            "{ id: $root.*.id, name: $root.*.name }"
        );
        assert_eq!(shaped_selection.selection(), &selection);
        assert_eq!(
            shaped_selection.output_shape().pretty_print(),
            "{ id: $root.*.id, name: $root.*.name }"
        );
        assert_eq!(&selection.shape(), shaped_selection.output_shape());
        {
            let refined_shaped_selection = shaped_selection.refine({
                let mut shapes = IndexMap::default();
                shapes.insert("$root".to_string(), Shape::empty_object());
                shapes
            });
            assert_eq!(
                refined_shaped_selection.pretty_print(),
                "{ id: None, name: None }"
            );
        }
        {
            let replaced_shaped_selection = shaped_selection.replace({
                let mut shapes = IndexMap::default();
                shapes.insert(
                    "$root".to_string(),
                    Shape::record({
                        let mut fields = Shape::empty_map();
                        fields.insert("id".to_string(), Shape::name("ID"));
                        fields.insert("name".to_string(), Shape::string());
                        fields
                    }),
                );
                shapes
            });
            assert_eq!(
                replaced_shaped_selection.pretty_print(),
                "{ id: ID, name: String }"
            );
        }
    }

    #[test]
    fn test_compute_output_shape() {
        assert_eq!(selection!("").shape().pretty_print(), "{}");

        assert_eq!(
            selection!("id name").shape().pretty_print(),
            "{ id: $root.*.id, name: $root.*.name }",
        );

        // // On hold until variadic $(...) is merged (PR #6456).
        // assert_eq!(
        //     selection!("$.data { thisOrThat: $(maybe.this, maybe.that) }")
        //         .shape()
        //         .pretty_print(),
        //     // Technically $.data could be an array, so this should be a union
        //     // of this shape and a list of this shape, except with
        //     // $root.*.data.0.maybe.{this,that} shape references.
        //     //
        //     // We could try to say that any { ... } shape represents either an
        //     // object or a list of objects, by policy, to avoid having to write
        //     // One<{...}, List<{...}>> everywhere a SubSelection appears.
        //     //
        //     // But then we don't know where the array indexes should go...
        //     "{ thisOrThat: One<$root.*.data.*.maybe.this, $root.*.data.*.maybe.that> }",
        // );

        assert_eq!(
            selection!(r#"
                id
                name
                friends: friend_ids { id: @ }
                alias: arrayOfArrays { x y }
                ys: arrayOfArrays.y xs: arrayOfArrays.x
            "#).shape().pretty_print(),

            // This output shape is wrong if $root.friend_ids turns out to be an
            // array, and it's tricky to see how to transform the shape to what
            // it would have been if we knew that, where friends: List<{ id:
            // $root.friend_ids.* }> (note the * meaning any array index),
            // because who's to say it's not the id field that should become the
            // List, rather than the friends field?
            "{ alias: { x: $root.*.arrayOfArrays.*.x, y: $root.*.arrayOfArrays.*.y }, friends: { id: $root.*.friend_ids.* }, id: $root.*.id, name: $root.*.name, xs: $root.*.arrayOfArrays.x, ys: $root.*.arrayOfArrays.y }",
        );

        assert_eq!(
            selection!(r#"
                id
                name
                friends: friend_ids->map({ id: @ })
                alias: arrayOfArrays { x y }
                ys: arrayOfArrays.y xs: arrayOfArrays.x
            "#).shape().pretty_print(),
            "{ alias: { x: $root.*.arrayOfArrays.*.x, y: $root.*.arrayOfArrays.*.y }, friends: List<{ id: $root.*.friend_ids.* }>, id: $root.*.id, name: $root.*.name, xs: $root.*.arrayOfArrays.x, ys: $root.*.arrayOfArrays.y }",
        );

        assert_eq!(
            selection!("$->echo({ thrice: [@, @, @] })")
                .shape()
                .pretty_print(),
            "{ thrice: [$root, $root, $root] }",
        );

        assert_eq!(
            selection!("$->echo({ thrice: [@, @, @] })->entries")
                .shape()
                .pretty_print(),
            "[{ key: \"thrice\", value: [$root, $root, $root] }]",
        );

        assert_eq!(
            selection!("$->echo({ thrice: [@, @, @] })->entries.key")
                .shape()
                .pretty_print(),
            "[\"thrice\"]",
        );

        assert_eq!(
            selection!("$->echo({ thrice: [@, @, @] })->entries.value")
                .shape()
                .pretty_print(),
            "[[$root, $root, $root]]",
        );

        assert_eq!(
            selection!("$->echo({ wrapped: @ })->entries { k: key v: value }")
                .shape()
                .pretty_print(),
            "[{ k: \"wrapped\", v: $root }]",
        );
    }
}
