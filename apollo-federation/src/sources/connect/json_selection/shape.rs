use std::sync::Arc;

use apollo_compiler::collections::IndexMap;
use shape::MergeSet;
use shape::Shape;
use shape::ShapeCase;
use shape::location::Located;
use shape::location::Location;
use shape::location::SourceId;

use super::JSONSelection;
use super::Key;
use super::NamedSelection;
use super::PathList;
use super::PathSelection;
use super::Ranged;
use super::SubSelection;
use super::known_var::KnownVariable;
use super::lit_expr::LitExpr;
use super::location::WithRange;
use super::methods::ArrowMethod;

impl JSONSelection {
    #[allow(dead_code)]
    pub(crate) fn into_shape(self) -> JSONShape {
        let input = JSONShapeInput {
            selection: Arc::new(self),
            named_shapes: IndexMap::default(),
        };

        let output = input.compute();

        JSONShape { input, output }
    }

    pub(crate) fn output_shape(&self, named_shapes: &IndexMap<String, Shape>) -> JSONShapeOutput {
        let input_shape = if let Some(root_shape) = named_shapes.get("$root") {
            root_shape.with_name("$root", [])
        } else {
            Shape::name("$root", [])
        };

        // At this level, $ and @ always start out the same.
        let dollar_shape = input_shape.clone();

        self.compute_output_shape(
            input_shape,
            dollar_shape,
            named_shapes,
            &SourceId::Other("JSONSelection".into()),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JSONShape {
    input: JSONShapeInput,
    output: JSONShapeOutput,
}

impl JSONShape {
    #[allow(dead_code)]
    pub(crate) fn lookup(
        &self,
        name: &str,
        locations: impl IntoIterator<Item = Location>,
    ) -> Shape {
        self.input.lookup(name, locations)
    }

    /// Add new named shapes to this [`JSONShape`], using [`Shape::all`] to
    /// combine shapes wherever there are collisions with existing shapes.
    #[allow(dead_code)]
    pub(crate) fn refine(&self, new_named_shapes: IndexMap<String, Shape>) -> Self {
        let mut new_input = self.input.clone();
        for (new_name, new_shape) in new_named_shapes {
            if let Some(old_shape) = new_input.named_shapes.get_mut(&new_name) {
                *old_shape = Shape::all([old_shape.clone(), new_shape], []);
            } else {
                new_input.named_shapes.insert(new_name, new_shape);
            }
        }

        let new_output = new_input.compute();

        Self {
            input: new_input,
            output: new_output,
        }
    }

    /// Add new named shapes to this [`JSONShape`], replacing any existing
    /// shapes with the same name.
    #[allow(dead_code)]
    pub(crate) fn replace(&self, new_named_shapes: IndexMap<String, Shape>) -> Self {
        let mut new_input = self.input.clone();
        for (new_name, new_shape) in new_named_shapes {
            new_input.named_shapes.insert(new_name, new_shape);
        }

        let new_output = new_input.compute();

        Self {
            input: new_input,
            output: new_output,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JSONShapeInput {
    pub(crate) selection: Arc<JSONSelection>,
    pub(crate) named_shapes: IndexMap<String, Shape>,
}

impl JSONShapeInput {
    #[allow(dead_code)]
    pub(crate) fn selection(&self) -> &JSONSelection {
        self.selection.as_ref()
    }

    #[allow(dead_code)]
    pub(crate) fn lookup(
        &self,
        name: &str,
        locations: impl IntoIterator<Item = Location>,
    ) -> Shape {
        if let Some(shape) = self.named_shapes.get(name) {
            shape.with_name(name, locations)
        } else {
            Shape::name(name, locations)
        }
    }

    pub(crate) fn compute(&self) -> JSONShapeOutput {
        self.selection.output_shape(&self.named_shapes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JSONShapeOutput {
    pub(crate) shape: Shape,
    pub(crate) names: MergeSet<shape::Name>,
}

impl JSONShapeOutput {
    pub(crate) fn new(shape: Shape, names: impl IntoIterator<Item = shape::Name>) -> Self {
        // TODO Process output shape for additional names?
        Self {
            shape,
            names: MergeSet::new(names),
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
        // A shared source name to use for all locations originating from this `JSONSelection`
        source_id: &SourceId,
    ) -> JSONShapeOutput;
}

impl ComputeOutputShape for JSONSelection {
    fn compute_output_shape(
        &self,
        input_shape: Shape,
        dollar_shape: Shape,
        named_shapes: &IndexMap<String, Shape>,
        source_id: &SourceId,
    ) -> JSONShapeOutput {
        match self {
            Self::Named(selection) => {
                selection.compute_output_shape(input_shape, dollar_shape, named_shapes, source_id)
            }
            Self::Path(path_selection) => path_selection.compute_output_shape(
                input_shape,
                dollar_shape,
                named_shapes,
                source_id,
            ),
        }
    }
}

impl<T: ComputeOutputShape> ComputeOutputShape for WithRange<T> {
    fn compute_output_shape(
        &self,
        input_shape: Shape,
        dollar_shape: Shape,
        named_shapes: &IndexMap<String, Shape>,
        source_id: &SourceId,
    ) -> JSONShapeOutput {
        self.as_ref()
            .compute_output_shape(input_shape, dollar_shape, named_shapes, source_id)
    }
}

impl ComputeOutputShape for NamedSelection {
    fn compute_output_shape(
        &self,
        input_shape: Shape,
        dollar_shape: Shape,
        named_shapes: &IndexMap<String, Shape>,
        source_id: &SourceId,
    ) -> JSONShapeOutput {
        let mut output = Shape::empty_map();
        let mut names = MergeSet::new([]);
        names.extend(input_shape.names().cloned());
        names.extend(dollar_shape.names().cloned());

        match self {
            Self::Field(alias_opt, key, selection) => {
                let output_key = alias_opt
                    .as_ref()
                    .map_or(key.as_str(), |alias| alias.name());
                let field_shape = field(&dollar_shape, key, source_id);
                output.insert(
                    output_key.to_string(),
                    if let Some(selection) = selection {
                        let selection_output = selection.compute_output_shape(
                            field_shape,
                            dollar_shape,
                            named_shapes,
                            source_id,
                        );
                        names.extend(selection_output.names);
                        selection_output.shape
                    } else {
                        names.extend(field_shape.names().cloned());
                        field_shape
                    },
                );
            }

            Self::Path { alias, path, .. } => {
                let path_output =
                    path.compute_output_shape(input_shape, dollar_shape, named_shapes, source_id);
                names.extend(path_output.names);
                if let Some(alias) = alias {
                    output.insert(alias.name().to_string(), path_output.shape);
                } else {
                    return JSONShapeOutput {
                        shape: path_output.shape,
                        names,
                    };
                }
            }

            Self::Group(alias, sub_selection) => {
                let sub_output = sub_selection.compute_output_shape(
                    input_shape,
                    dollar_shape,
                    named_shapes,
                    source_id,
                );
                names.extend(sub_output.names);
                output.insert(alias.name().to_string(), sub_output.shape);
            }
        };

        JSONShapeOutput {
            shape: Shape::object(output, Shape::none(), self.shape_location(source_id)),
            names,
        }
    }
}

impl ComputeOutputShape for PathSelection {
    fn compute_output_shape(
        &self,
        input_shape: Shape,
        dollar_shape: Shape,
        named_shapes: &IndexMap<String, Shape>,
        source_id: &SourceId,
    ) -> JSONShapeOutput {
        match self.path.as_ref() {
            PathList::Key(_, _) => {
                // If this is a KeyPath, we need to evaluate the path starting
                // from the current $ shape, so we pass dollar_shape as the data
                // *and* dollar_shape to self.path.compute_output_shape.
                self.path.compute_output_shape(
                    dollar_shape.clone(),
                    dollar_shape.clone(),
                    named_shapes,
                    source_id,
                )
            }
            // If this is not a KeyPath, keep evaluating against input_shape.
            // This logic parallels PathSelection::apply_to_path (above).
            _ => self
                .path
                .compute_output_shape(input_shape, dollar_shape, named_shapes, source_id),
        }
    }
}

impl ComputeOutputShape for PathList {
    fn compute_output_shape(
        &self,
        input_shape: Shape,
        dollar_shape: Shape,
        named_shapes: &IndexMap<String, Shape>,
        source_id: &SourceId,
    ) -> JSONShapeOutput {
        let mut names = MergeSet::new([]);
        names.extend(input_shape.names().cloned());
        names.extend(dollar_shape.names().cloned());

        fn finish(shape: Shape, names: MergeSet<shape::Name>) -> JSONShapeOutput {
            JSONShapeOutput { shape, names }
        }

        match self {
            PathList::Var(ranged_var_name, tail) => {
                let var_name = ranged_var_name.as_ref();
                let var_shape = if var_name == &KnownVariable::AtSign {
                    input_shape
                } else if var_name == &KnownVariable::Dollar {
                    dollar_shape.clone()
                } else if let Some(shape) = named_shapes.get(var_name.as_str()) {
                    shape.clone()
                } else {
                    Shape::name(var_name.as_str(), ranged_var_name.shape_location(source_id))
                };

                let output =
                    tail.compute_output_shape(var_shape, dollar_shape, named_shapes, source_id);
                names.extend(output.names);
                finish(output.shape, names)
            }

            PathList::Key(key, rest) => {
                // If this is the first key in the path,
                // PathSelection::compute_output_shape will have set our
                // input_shape equal to its dollar_shape, thereby ensuring that
                // some.nested.path is equivalent to $.some.nested.path.
                if input_shape.is_none() {
                    // Following WithRange<PathList>::apply_to_path, we do not
                    // want to call rest.compute_output_shape recursively with
                    // an input data shape corresponding to missing data, though
                    // it might do the right thing.
                    return finish(input_shape, names);
                }

                if let ShapeCase::Array { prefix, tail } = input_shape.case() {
                    // Map rest.compute_output_shape over the prefix and rest
                    // elements of the array shape, so we don't have to map
                    // array shapes for the other PathList variants.
                    let mapped_prefix = prefix
                        .iter()
                        .map(|shape| {
                            if shape.is_none() {
                                names.extend(shape.names().cloned());
                                shape.clone()
                            } else {
                                let rest_output = rest.compute_output_shape(
                                    field(shape, key, source_id),
                                    dollar_shape.clone(),
                                    named_shapes,
                                    source_id,
                                );
                                names.extend(rest_output.names);
                                rest_output.shape
                            }
                        })
                        .collect::<Vec<_>>();

                    let mapped_rest = if tail.is_none() {
                        names.extend(tail.names().cloned());
                        tail.clone()
                    } else {
                        let rest_output = rest.compute_output_shape(
                            field(tail, key, source_id),
                            dollar_shape.clone(),
                            named_shapes,
                            source_id,
                        );
                        names.extend(rest_output.names);
                        rest_output.shape
                    };

                    finish(
                        Shape::array(mapped_prefix, mapped_rest, input_shape.locations().cloned()),
                        names,
                    )
                } else {
                    let rest_output = rest.compute_output_shape(
                        field(&input_shape, key, source_id),
                        dollar_shape.clone(),
                        named_shapes,
                        source_id,
                    );
                    names.extend(rest_output.names);
                    finish(rest_output.shape, names)
                }
            }

            PathList::Expr(expr, tail) => {
                let expr_output = expr.compute_output_shape(
                    input_shape,
                    dollar_shape.clone(),
                    named_shapes,
                    source_id,
                );
                names.extend(expr_output.names);

                let tail_output = tail.compute_output_shape(
                    expr_output.shape,
                    dollar_shape.clone(),
                    named_shapes,
                    source_id,
                );
                names.extend(tail_output.names);

                finish(tail_output.shape, names)
            }

            PathList::Method(method_name, _method_args, _tail) => {
                if let Some(_method) = ArrowMethod::lookup(method_name.as_str()) {
                    // TODO: call method.shape here to re-enable method type-checking
                    //  call for each inner type of a One
                    finish(Shape::unknown(method_name.shape_location(source_id)), names)
                } else {
                    let message = format!("Method ->{} not found", method_name.as_str());
                    finish(
                        Shape::error(message.as_str(), method_name.shape_location(source_id)),
                        names,
                    )
                }
            }

            PathList::Selection(selection) => {
                let output = selection.compute_output_shape(
                    input_shape,
                    dollar_shape,
                    named_shapes,
                    source_id,
                );
                names.extend(output.names);
                finish(output.shape, names)
            }

            PathList::Empty => finish(input_shape, names),
        }
    }
}

impl ComputeOutputShape for WithRange<LitExpr> {
    fn compute_output_shape(
        &self,
        input_shape: Shape,
        dollar_shape: Shape,
        named_shapes: &IndexMap<String, Shape>,
        source_id: &SourceId,
    ) -> JSONShapeOutput {
        let locations = self.shape_location(source_id);

        let mut names = MergeSet::new([]);
        names.extend(input_shape.names().cloned());
        names.extend(dollar_shape.names().cloned());

        fn finish(shape: Shape, names: MergeSet<shape::Name>) -> JSONShapeOutput {
            JSONShapeOutput { shape, names }
        }

        match self.as_ref() {
            LitExpr::Null => finish(Shape::null(locations), names),
            LitExpr::Bool(value) => finish(Shape::bool_value(*value, locations), names),
            LitExpr::String(value) => finish(Shape::string_value(value.as_str(), locations), names),

            LitExpr::Number(value) => finish(
                {
                    if let Some(n) = value.as_i64() {
                        Shape::int_value(n, locations)
                    } else if value.is_f64() {
                        Shape::float(locations)
                    } else {
                        Shape::error("Number neither Int nor Float", locations)
                    }
                },
                names,
            ),

            LitExpr::Object(map) => {
                let mut fields = Shape::empty_map();
                for (key, value) in map {
                    let output = value.compute_output_shape(
                        input_shape.clone(),
                        dollar_shape.clone(),
                        named_shapes,
                        source_id,
                    );
                    names.extend(output.names);
                    fields.insert(key.as_string(), output.shape);
                }

                finish(Shape::object(fields, Shape::none(), locations), names)
            }

            LitExpr::Array(vec) => {
                let mut shapes = Vec::with_capacity(vec.len());

                for value in vec {
                    let output = value.compute_output_shape(
                        input_shape.clone(),
                        dollar_shape.clone(),
                        named_shapes,
                        source_id,
                    );

                    names.extend(output.names);

                    shapes.push(output.shape);
                }

                finish(Shape::array(shapes, Shape::none(), locations), names)
            }

            LitExpr::Path(path) => {
                let output =
                    path.compute_output_shape(input_shape, dollar_shape, named_shapes, source_id);
                names.extend(output.names);
                finish(output.shape, names)
            }
        }
    }
}

impl ComputeOutputShape for SubSelection {
    fn compute_output_shape(
        &self,
        input_shape: Shape,
        _previous_dollar_shape: Shape,
        named_shapes: &IndexMap<String, Shape>,
        source_id: &SourceId,
    ) -> JSONShapeOutput {
        let mut names = MergeSet::new([]);
        names.extend(input_shape.names().cloned());

        // Just as SubSelection::apply_to_path calls apply_to_array when data is
        // an array, so compute_output_shape recursively computes the output
        // shapes of each array element shape.
        if let ShapeCase::Array { prefix, tail } = input_shape.case() {
            let new_prefix = prefix
                .iter()
                .map(|shape| {
                    let output = self.compute_output_shape(
                        shape.clone(),
                        shape.clone(),
                        named_shapes,
                        source_id,
                    );
                    names.extend(output.names);
                    output.shape
                })
                .collect::<Vec<_>>();

            let new_tail = if tail.is_none() {
                tail.clone()
            } else {
                let output =
                    self.compute_output_shape(tail.clone(), tail.clone(), named_shapes, source_id);
                names.extend(output.names);
                output.shape
            };

            return JSONShapeOutput {
                names,
                shape: Shape::array(new_prefix, new_tail, self.shape_location(source_id)),
            };
        }

        // If the input shape is a named shape, it might end up being an array,
        // so we need to hedge the output shape using a wildcard that maps over
        // array elements.
        let input_shape = input_shape.any_item(Vec::new());

        // The SubSelection rebinds the $ variable to the selected input object,
        // so we can ignore _previous_dollar_shape.
        let dollar_shape = input_shape.clone();

        // Build up the merged object shape using Shape::all to merge the
        // individual named_selection object shapes.
        let mut all_shape = Shape::empty_object(self.shape_location(source_id));

        for named_selection in self.selections.iter() {
            let named_output = named_selection.compute_output_shape(
                input_shape.clone(),
                dollar_shape.clone(),
                named_shapes,
                source_id,
            );

            names.extend(named_output.names);

            // Simplifying as we go with Shape::all keeps all_shape relatively
            // small in the common case when all named_selection items return an
            // object shape, since those object shapes can all be merged
            // together into one object.
            all_shape = Shape::all(
                [all_shape, named_output.shape],
                self.shape_location(source_id),
            );

            // If any named_selection item returns null instead of an object,
            // that nullifies the whole object and allows shape computation to
            // bail out early.
            if all_shape.is_null() {
                break;
            }
        }

        JSONShapeOutput {
            names,
            shape: all_shape,
        }
    }
}

/// Helper to get the field from a shape or error if the object doesn't have that field.
fn field(shape: &Shape, key: &WithRange<Key>, source_id: &SourceId) -> Shape {
    if let ShapeCase::One(inner) = shape.case() {
        let mut new_fields = Vec::new();
        for inner_field in inner.iter() {
            new_fields.push(field(inner_field, key, source_id));
        }
        return Shape::one(new_fields, shape.locations().cloned());
    }
    if shape.is_none() || shape.is_null() {
        return Shape::none();
    }
    let field_shape = shape.field(key.as_str(), key.shape_location(source_id));
    if field_shape.is_none() {
        return Shape::error(
            format!("field `{field}` not found", field = key.as_str()),
            key.shape_location(source_id),
        );
    }
    field_shape
}
