/// ApplyTo is a trait for applying a JSONSelection to a JSON value, collecting
/// any/all errors encountered in the process.
use std::hash::Hash;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use serde_json_bytes::Map as JSONMap;
use serde_json_bytes::Value as JSON;
use serde_json_bytes::json;
use shape::Shape;
use shape::ShapeCase;
use shape::location::SourceId;

use super::helpers::json_merge;
use super::helpers::json_type_name;
use super::immutable::InputPath;
use super::known_var::KnownVariable;
use super::lit_expr::LitExpr;
use super::location::OffsetRange;
use super::location::Ranged;
use super::location::WithRange;
use super::methods::ArrowMethod;
use super::parser::*;

pub(super) type VarsWithPathsMap<'a> = IndexMap<KnownVariable, (&'a JSON, InputPath<JSON>)>;

impl JSONSelection {
    // Applying a selection to a JSON value produces a new JSON value, along
    // with any/all errors encountered in the process. The value is represented
    // as an Option to allow for undefined/missing values (which JSON does not
    // explicitly support), which are distinct from null values (which it does
    // support).
    pub fn apply_to(&self, data: &JSON) -> (Option<JSON>, Vec<ApplyToError>) {
        self.apply_with_vars(data, &IndexMap::default())
    }

    pub fn apply_with_vars(
        &self,
        data: &JSON,
        vars: &IndexMap<String, JSON>,
    ) -> (Option<JSON>, Vec<ApplyToError>) {
        // Using IndexSet over HashSet to preserve the order of the errors.
        let mut errors = IndexSet::default();

        let mut vars_with_paths: VarsWithPathsMap = IndexMap::default();
        for (var_name, var_data) in vars {
            vars_with_paths.insert(
                KnownVariable::from_str(var_name.as_str()),
                (var_data, InputPath::empty().append(json!(var_name))),
            );
        }
        // The $ variable initially refers to the root data value, but is
        // rebound by nested selection sets to refer to the root value the
        // selection set was applied to.
        vars_with_paths.insert(KnownVariable::Dollar, (data, InputPath::empty()));

        let (value, apply_errors) = self.apply_to_path(data, &vars_with_paths, &InputPath::empty());

        // Since errors is an IndexSet, this line effectively deduplicates the
        // errors, in an attempt to make them less verbose. However, now that we
        // include both path and range information in the errors, there's an
        // argument to be made that errors can no longer be meaningfully
        // deduplicated, so we might consider sticking with a Vec<ApplyToError>.
        errors.extend(apply_errors);

        (value, errors.into_iter().collect())
    }

    pub fn shape(&self) -> Shape {
        self.compute_output_shape(
            // If we don't know anything about the shape of the input data, we
            // can represent the data symbolically using the $root variable
            // shape. Subproperties needed from this shape will show up as
            // subpaths like $root.books.4.isbn in the output shape.
            //
            // While we do not currently have a $root variable available as a
            // KnownVariable during apply_to_path execution, we might consider
            // adding it, since it would align with the way we process other
            // variable shapes. For now, $root exists only as a shape name that
            // we are inventing right here.
            Shape::name("$root", Vec::new()),
            // If we wanted to specify anything about the shape of the $root
            // variable, we could define a shape for "$root" in this map.
            &IndexMap::default(),
            &SourceId::Other("JSONSelection".into()),
        )
    }

    pub fn compute_output_shape(
        &self,
        input_shape: Shape,
        named_var_shapes: &IndexMap<&str, Shape>,
        source_id: &SourceId,
    ) -> Shape {
        match self {
            Self::Named(selection) => selection.compute_output_shape(
                input_shape.clone(),
                input_shape,
                named_var_shapes,
                source_id,
            ),
            Self::Path(path_selection) => path_selection.compute_output_shape(
                input_shape.clone(),
                input_shape,
                named_var_shapes,
                source_id,
            ),
        }
    }
}

pub(super) trait ApplyToInternal {
    // This is the trait method that should be implemented and called
    // recursively by the various JSONSelection types.
    fn apply_to_path(
        &self,
        data: &JSON,
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
    ) -> (Option<JSON>, Vec<ApplyToError>);

    // When array is encountered, the Self selection will be applied to each
    // element of the array, producing a new array.
    fn apply_to_array(
        &self,
        data_array: &[JSON],
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
    ) -> (Option<JSON>, Vec<ApplyToError>) {
        let mut output = Vec::with_capacity(data_array.len());
        let mut errors = Vec::new();

        for (i, element) in data_array.iter().enumerate() {
            let input_path_with_index = input_path.append(json!(i));
            let (applied, apply_errors) = self.apply_to_path(element, vars, &input_path_with_index);
            errors.extend(apply_errors);
            // When building an Object, we can simply omit missing properties
            // and report an error, but when building an Array, we need to
            // insert null values to preserve the original array indices/length.
            output.push(applied.unwrap_or(JSON::Null));
        }

        (Some(JSON::Array(output)), errors)
    }

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
        named_var_shapes: &IndexMap<&str, Shape>,
        // A shared source name to use for all locations originating from this `JSONSelection`
        source_id: &SourceId,
    ) -> Shape;
}

#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub struct ApplyToError {
    message: String,
    path: Vec<JSON>,
    range: OffsetRange,
}

impl ApplyToError {
    pub(crate) const fn new(message: String, path: Vec<JSON>, range: OffsetRange) -> Self {
        Self {
            message,
            path,
            range,
        }
    }

    // This macro is useful for tests, but it absolutely should never be used with
    // dynamic input at runtime, since it panics for any input that's not JSON.
    #[cfg(test)]
    pub(crate) fn from_json(json: &JSON) -> Self {
        let error = json.as_object().unwrap();
        let message = error.get("message").unwrap().as_str().unwrap().to_string();
        let path = error.get("path").unwrap().as_array().unwrap().clone();
        let range = error.get("range").unwrap().as_array().unwrap();

        Self {
            message,
            path,
            range: if range.len() == 2 {
                let start = range[0].as_u64().unwrap() as usize;
                let end = range[1].as_u64().unwrap() as usize;
                Some(start..end)
            } else {
                None
            },
        }
    }

    pub fn message(&self) -> &str {
        self.message.as_str()
    }

    pub fn path(&self) -> &[JSON] {
        self.path.as_slice()
    }

    pub fn range(&self) -> OffsetRange {
        self.range.clone()
    }
}

// Rust doesn't allow implementing methods directly on tuples like
// (Option<JSON>, Vec<ApplyToError>), so we define a trait to provide the
// methods we need, and implement the trait for the tuple in question.
pub(super) trait ApplyToResultMethods {
    fn prepend_errors(self, errors: Vec<ApplyToError>) -> Self;

    fn and_then_collecting_errors(
        self,
        f: impl FnOnce(&JSON) -> (Option<JSON>, Vec<ApplyToError>),
    ) -> (Option<JSON>, Vec<ApplyToError>);
}

impl ApplyToResultMethods for (Option<JSON>, Vec<ApplyToError>) {
    // Intentionally taking ownership of self to avoid cloning, since we pretty
    // much always use this method to replace the previous (value, errors) tuple
    // before returning.
    fn prepend_errors(self, mut errors: Vec<ApplyToError>) -> Self {
        if errors.is_empty() {
            self
        } else {
            let (value_opt, apply_errors) = self;
            errors.extend(apply_errors);
            (value_opt, errors)
        }
    }

    // A substitute for Option<_>::and_then that accumulates errors behind the
    // scenes. I'm no Haskell programmer, but this feels monadic? ¯\_(ツ)_/¯
    fn and_then_collecting_errors(
        self,
        f: impl FnOnce(&JSON) -> (Option<JSON>, Vec<ApplyToError>),
    ) -> (Option<JSON>, Vec<ApplyToError>) {
        match self {
            (Some(data), errors) => f(&data).prepend_errors(errors),
            (None, errors) => (None, errors),
        }
    }
}

impl ApplyToInternal for JSONSelection {
    fn apply_to_path(
        &self,
        data: &JSON,
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
    ) -> (Option<JSON>, Vec<ApplyToError>) {
        match self {
            // Because we represent a JSONSelection::Named as a SubSelection, we
            // can fully delegate apply_to_path to SubSelection::apply_to_path.
            // Even if we represented Self::Named as a Vec<NamedSelection>, we
            // could still delegate to SubSelection::apply_to_path, but we would
            // need to create a temporary SubSelection to wrap the selections
            // Vec.
            Self::Named(named_selections) => named_selections.apply_to_path(data, vars, input_path),
            Self::Path(path_selection) => path_selection.apply_to_path(data, vars, input_path),
        }
    }

    fn compute_output_shape(
        &self,
        input_shape: Shape,
        dollar_shape: Shape,
        named_var_shapes: &IndexMap<&str, Shape>,
        source_id: &SourceId,
    ) -> Shape {
        match self {
            Self::Named(selection) => selection.compute_output_shape(
                input_shape,
                dollar_shape,
                named_var_shapes,
                source_id,
            ),
            Self::Path(path_selection) => path_selection.compute_output_shape(
                input_shape,
                dollar_shape,
                named_var_shapes,
                source_id,
            ),
        }
    }
}

impl ApplyToInternal for NamedSelection {
    fn apply_to_path(
        &self,
        data: &JSON,
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
    ) -> (Option<JSON>, Vec<ApplyToError>) {
        let mut output: Option<JSON> = None;
        let mut errors = Vec::new();

        match self {
            Self::Field(alias, key, selection) => {
                let input_path_with_key = input_path.append(key.to_json());
                let name = key.as_str();
                if let Some(child) = data.get(name) {
                    let output_name = alias.as_ref().map_or(name, |alias| alias.name());
                    if let Some(selection) = selection {
                        let (value, apply_errors) =
                            selection.apply_to_path(child, vars, &input_path_with_key);
                        errors.extend(apply_errors);
                        if let Some(value) = value {
                            output = Some(json!({ output_name: value }));
                        }
                    } else {
                        output = Some(json!({ output_name: child.clone() }));
                    }
                } else {
                    errors.push(ApplyToError::new(
                        format!(
                            "Property {} not found in {}",
                            key.dotted(),
                            json_type_name(data),
                        ),
                        input_path_with_key.to_vec(),
                        key.range(),
                    ));
                }
            }
            Self::Path {
                alias,
                path,
                inline,
            } => {
                let (value_opt, apply_errors) = path.apply_to_path(data, vars, input_path);
                errors.extend(apply_errors);

                if let Some(alias) = alias {
                    // Handle the NamedPathSelection case.
                    if let Some(value) = value_opt {
                        output = Some(json!({ alias.name(): value }));
                    }
                } else if *inline {
                    match value_opt {
                        Some(JSON::Object(map)) => {
                            output = Some(JSON::Object(map));
                        }
                        Some(JSON::Null) => {
                            output = Some(JSON::Null);
                        }
                        Some(value) => {
                            errors.push(ApplyToError::new(
                                format!("Expected object or null, not {}", json_type_name(&value)),
                                input_path.to_vec(),
                                path.range(),
                            ));
                        }
                        None => {
                            errors.push(ApplyToError::new(
                                "Expected object or null, not nothing".to_string(),
                                input_path.to_vec(),
                                path.range(),
                            ));
                        }
                    }
                } else {
                    errors.push(ApplyToError::new(
                        "Named path must have an alias, a trailing subselection, or be inlined with ... and produce an object or null".to_string(),
                        input_path.to_vec(),
                        path.range(),
                    ));
                }
            }
            Self::Group(alias, sub_selection) => {
                let (value_opt, apply_errors) = sub_selection.apply_to_path(data, vars, input_path);
                errors.extend(apply_errors);
                if let Some(value) = value_opt {
                    output = Some(json!({ alias.name(): value }));
                }
            }
        };

        (output, errors)
    }

    fn compute_output_shape(
        &self,
        input_shape: Shape,
        dollar_shape: Shape,
        named_var_shapes: &IndexMap<&str, Shape>,
        source_id: &SourceId,
    ) -> Shape {
        let mut output = Shape::empty_map();

        match self {
            Self::Field(alias_opt, key, selection) => {
                let output_key = alias_opt
                    .as_ref()
                    .map_or(key.as_str(), |alias| alias.name());
                let field_shape = field(&dollar_shape, key, source_id);
                output.insert(
                    output_key.to_string(),
                    if let Some(selection) = selection {
                        selection.compute_output_shape(
                            field_shape,
                            dollar_shape,
                            named_var_shapes,
                            source_id,
                        )
                    } else {
                        field_shape
                    },
                );
            }
            Self::Path { alias, path, .. } => {
                let path_shape = path.compute_output_shape(
                    input_shape,
                    dollar_shape,
                    named_var_shapes,
                    source_id,
                );
                if let Some(alias) = alias {
                    output.insert(alias.name().to_string(), path_shape);
                } else {
                    return path_shape;
                }
            }
            Self::Group(alias, sub_selection) => {
                output.insert(
                    alias.name().to_string(),
                    sub_selection.compute_output_shape(
                        input_shape,
                        dollar_shape,
                        named_var_shapes,
                        source_id,
                    ),
                );
            }
        };

        Shape::object(output, Shape::none(), self.shape_location(source_id))
    }
}

impl ApplyToInternal for PathSelection {
    fn apply_to_path(
        &self,
        data: &JSON,
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
    ) -> (Option<JSON>, Vec<ApplyToError>) {
        match (self.path.as_ref(), vars.get(&KnownVariable::Dollar)) {
            // If this is a KeyPath, instead of using data as given, we need to
            // evaluate the path starting from the current value of $. To evaluate
            // the KeyPath against data, prefix it with @. This logic supports
            // method chaining like obj->has('a')->and(obj->has('b')), where both
            // obj references are interpreted as $.obj.
            (PathList::Key(_, _), Some((dollar_data, dollar_path))) => {
                self.path.apply_to_path(dollar_data, vars, dollar_path)
            }

            // If $ is undefined for some reason, fall back to using data...
            // TODO: Since $ should never be undefined, we might want to
            // guarantee its existence at compile time, somehow.
            // (PathList::Key(_, _), None) => todo!(),
            _ => self.path.apply_to_path(data, vars, input_path),
        }
    }

    fn compute_output_shape(
        &self,
        input_shape: Shape,
        dollar_shape: Shape,
        named_var_shapes: &IndexMap<&str, Shape>,
        source_id: &SourceId,
    ) -> Shape {
        match self.path.as_ref() {
            PathList::Key(_, _) => {
                // If this is a KeyPath, we need to evaluate the path starting
                // from the current $ shape, so we pass dollar_shape as the data
                // *and* dollar_shape to self.path.compute_output_shape.
                self.path.compute_output_shape(
                    dollar_shape.clone(),
                    dollar_shape,
                    named_var_shapes,
                    source_id,
                )
            }
            // If this is not a KeyPath, keep evaluating against input_shape.
            // This logic parallels PathSelection::apply_to_path (above).
            _ => self.path.compute_output_shape(
                input_shape,
                dollar_shape,
                named_var_shapes,
                source_id,
            ),
        }
    }
}

impl ApplyToInternal for WithRange<PathList> {
    fn apply_to_path(
        &self,
        data: &JSON,
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
    ) -> (Option<JSON>, Vec<ApplyToError>) {
        match self.as_ref() {
            PathList::Var(ranged_var_name, tail) => {
                let var_name = ranged_var_name.as_ref();
                if var_name == &KnownVariable::AtSign {
                    // We represent @ as a variable name in PathList::Var, but
                    // it is never stored in the vars map, because it is always
                    // shorthand for the current data value.
                    tail.apply_to_path(data, vars, input_path)
                } else if let Some((var_data, var_path)) = vars.get(var_name) {
                    // Variables are associated with a path, which is always
                    // just the variable name for named $variables other than $.
                    // For the special variable $, the path represents the
                    // sequence of keys from the root input data to the $ data.
                    tail.apply_to_path(var_data, vars, var_path)
                } else {
                    (
                        None,
                        vec![ApplyToError::new(
                            format!("Variable {} not found", var_name.as_str()),
                            input_path.to_vec(),
                            ranged_var_name.range(),
                        )],
                    )
                }
            }
            PathList::Key(key, tail) => {
                let input_path_with_key = input_path.append(key.to_json());

                if let JSON::Array(array) = data {
                    // If we recursively call self.apply_to_array, it will end
                    // up invoking the tail of the key recursively, whereas we
                    // want to apply the tail once to the entire output array of
                    // shallow key lookups. To keep the recursion shallow, we
                    // need a version of self that has the same key but no tail.
                    let empty_tail = WithRange::new(PathList::Empty, tail.range());
                    let self_with_empty_tail =
                        WithRange::new(PathList::Key(key.clone(), empty_tail), key.range());

                    self_with_empty_tail
                        .apply_to_array(array, vars, input_path)
                        .and_then_collecting_errors(|shallow_mapped_array| {
                            // This tail.apply_to_path call happens only once,
                            // passing to the original/top-level tail the entire
                            // array produced by key-related recursion/mapping.
                            tail.apply_to_path(shallow_mapped_array, vars, &input_path_with_key)
                        })
                } else {
                    if !matches!(data, JSON::Object(_)) {
                        return (
                            None,
                            vec![ApplyToError::new(
                                format!(
                                    "Property {} not found in {}",
                                    key.dotted(),
                                    json_type_name(data),
                                ),
                                input_path_with_key.to_vec(),
                                key.range(),
                            )],
                        );
                    }
                    let Some(child) = data.get(key.as_str()) else {
                        return (
                            None,
                            vec![ApplyToError::new(
                                format!(
                                    "Property {} not found in {}",
                                    key.dotted(),
                                    json_type_name(data),
                                ),
                                input_path_with_key.to_vec(),
                                key.range(),
                            )],
                        );
                    };
                    tail.apply_to_path(child, vars, &input_path_with_key)
                }
            }
            PathList::Expr(expr, tail) => expr
                .apply_to_path(data, vars, input_path)
                .and_then_collecting_errors(|value| tail.apply_to_path(value, vars, input_path)),
            PathList::Method(method_name, method_args, tail) => {
                let method_path =
                    input_path.append(JSON::String(format!("->{}", method_name.as_ref()).into()));

                ArrowMethod::lookup(method_name).map_or_else(
                    || {
                        (
                            None,
                            vec![ApplyToError::new(
                                format!("Method ->{} not found", method_name.as_ref()),
                                method_path.to_vec(),
                                method_name.range(),
                            )],
                        )
                    },
                    |method| {
                        let (result_opt, errors) = method.apply(
                            method_name,
                            method_args.as_ref(),
                            data,
                            vars,
                            &method_path,
                        );

                        if let Some(result) = result_opt {
                            tail.apply_to_path(&result, vars, &method_path)
                                .prepend_errors(errors)
                        } else {
                            // If the method produced no output, assume the errors
                            // explain the None. Methods can legitimately produce
                            // None without errors (like ->first or ->last on an
                            // empty array), so we do not report any blanket error
                            // here when errors.is_empty().
                            (None, errors)
                        }
                    },
                )
            }
            PathList::Selection(selection) => selection.apply_to_path(data, vars, input_path),
            PathList::Question(continuation) => {
                // Universal null check for any operation after ?
                if data.is_null() {
                    return (Some(JSON::Null), vec![]);
                }

                // If not null, continue with the wrapped operation
                let (result, mut errors) = continuation.apply_to_path(data, vars, input_path);

                // Post-process errors to add ? prefix for method errors
                if let PathList::Method(_, _, _) = continuation.as_ref() {
                    for error in &mut errors {
                        if error.message().starts_with("Method ->") {
                            error.message = error.message().replace("Method ->", "Method ?->");
                        }
                    }
                }

                (result, errors)
            }
            PathList::Empty => {
                // If data is not an object here, we want to preserve its value
                // without an error.
                (Some(data.clone()), vec![])
            }
        }
    }

    fn compute_output_shape(
        &self,
        input_shape: Shape,
        dollar_shape: Shape,
        named_var_shapes: &IndexMap<&str, Shape>,
        source_id: &SourceId,
    ) -> Shape {
        match self.as_ref() {
            PathList::Var(ranged_var_name, tail) => {
                let var_name = ranged_var_name.as_ref();
                let var_shape = if var_name == &KnownVariable::AtSign {
                    input_shape
                } else if var_name == &KnownVariable::Dollar {
                    dollar_shape.clone()
                } else if let Some(shape) = named_var_shapes.get(var_name.as_str()) {
                    shape.clone()
                } else {
                    Shape::name(var_name.as_str(), ranged_var_name.shape_location(source_id))
                };
                tail.compute_output_shape(var_shape, dollar_shape, named_var_shapes, source_id)
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
                    return input_shape;
                }

                if let ShapeCase::Array { prefix, tail } = input_shape.case() {
                    // Map rest.compute_output_shape over the prefix and rest
                    // elements of the array shape, so we don't have to map
                    // array shapes for the other PathList variants.
                    let mapped_prefix = prefix
                        .iter()
                        .map(|shape| {
                            if shape.is_none() {
                                shape.clone()
                            } else {
                                rest.compute_output_shape(
                                    field(shape, key, source_id),
                                    dollar_shape.clone(),
                                    named_var_shapes,
                                    source_id,
                                )
                            }
                        })
                        .collect::<Vec<_>>();

                    let mapped_rest = if tail.is_none() {
                        tail.clone()
                    } else {
                        rest.compute_output_shape(
                            field(tail, key, source_id),
                            dollar_shape,
                            named_var_shapes,
                            source_id,
                        )
                    };

                    Shape::array(mapped_prefix, mapped_rest, input_shape.locations)
                } else {
                    rest.compute_output_shape(
                        field(&input_shape, key, source_id),
                        dollar_shape,
                        named_var_shapes,
                        source_id,
                    )
                }
            }

            PathList::Expr(expr, tail) => tail.compute_output_shape(
                expr.compute_output_shape(
                    input_shape,
                    dollar_shape.clone(),
                    named_var_shapes,
                    source_id,
                ),
                dollar_shape,
                named_var_shapes,
                source_id,
            ),

            PathList::Method(method_name, _method_args, _tail) => ArrowMethod::lookup(method_name)
                .map_or_else(
                    || {
                        Shape::error(
                            format!("Method ->{} not found", method_name.as_str()),
                            method_name.shape_location(source_id),
                        )
                    },
                    |_method| Shape::unknown(method_name.shape_location(source_id)),
                ),

            PathList::Selection(selection) => selection.compute_output_shape(
                input_shape,
                dollar_shape,
                named_var_shapes,
                source_id,
            ),

            PathList::Question(continuation) => {
                // Optional operation always produces nullable output
                let result_shape = continuation.compute_output_shape(
                    input_shape,
                    dollar_shape,
                    named_var_shapes,
                    source_id,
                );
                // Make result nullable since optional chaining can produce null
                Shape::one([result_shape, Shape::none()], vec![])
            }

            PathList::Empty => input_shape,
        }
    }
}

impl ApplyToInternal for WithRange<LitExpr> {
    fn apply_to_path(
        &self,
        data: &JSON,
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
    ) -> (Option<JSON>, Vec<ApplyToError>) {
        match self.as_ref() {
            LitExpr::String(s) => (Some(JSON::String(s.clone().into())), vec![]),
            LitExpr::Number(n) => (Some(JSON::Number(n.clone())), vec![]),
            LitExpr::Bool(b) => (Some(JSON::Bool(*b)), vec![]),
            LitExpr::Null => (Some(JSON::Null), vec![]),
            LitExpr::Object(map) => {
                let mut output = JSONMap::with_capacity(map.len());
                let mut errors = Vec::new();
                for (key, value) in map {
                    let (value_opt, apply_errors) = value.apply_to_path(data, vars, input_path);
                    errors.extend(apply_errors);
                    if let Some(value_json) = value_opt {
                        output.insert(key.as_str(), value_json);
                    }
                }
                (Some(JSON::Object(output)), errors)
            }
            LitExpr::Array(vec) => {
                let mut output = Vec::with_capacity(vec.len());
                let mut errors = Vec::new();
                for value in vec {
                    let (value_opt, apply_errors) = value.apply_to_path(data, vars, input_path);
                    errors.extend(apply_errors);
                    output.push(value_opt.unwrap_or(JSON::Null));
                }
                (Some(JSON::Array(output)), errors)
            }
            LitExpr::Path(path) => path.apply_to_path(data, vars, input_path),
            LitExpr::LitPath(literal, subpath) => literal
                .apply_to_path(data, vars, input_path)
                .and_then_collecting_errors(|value| subpath.apply_to_path(value, vars, input_path)),
        }
    }

    fn compute_output_shape(
        &self,
        input_shape: Shape,
        dollar_shape: Shape,
        named_var_shapes: &IndexMap<&str, Shape>,
        source_id: &SourceId,
    ) -> Shape {
        let locations = self.shape_location(source_id);

        match self.as_ref() {
            LitExpr::Null => Shape::null(locations),
            LitExpr::Bool(value) => Shape::bool_value(*value, locations),
            LitExpr::String(value) => Shape::string_value(value.as_str(), locations),

            LitExpr::Number(value) => {
                if let Some(n) = value.as_i64() {
                    Shape::int_value(n, locations)
                } else if value.is_f64() {
                    Shape::float(locations)
                } else {
                    Shape::error("Number neither Int nor Float", locations)
                }
            }

            LitExpr::Object(map) => {
                let mut fields = Shape::empty_map();
                for (key, value) in map {
                    fields.insert(
                        key.as_string(),
                        value.compute_output_shape(
                            input_shape.clone(),
                            dollar_shape.clone(),
                            named_var_shapes,
                            source_id,
                        ),
                    );
                }
                Shape::object(fields, Shape::none(), locations)
            }

            LitExpr::Array(vec) => {
                let mut shapes = Vec::with_capacity(vec.len());
                for value in vec {
                    shapes.push(value.compute_output_shape(
                        input_shape.clone(),
                        dollar_shape.clone(),
                        named_var_shapes,
                        source_id,
                    ));
                }
                Shape::array(shapes, Shape::none(), locations)
            }

            LitExpr::Path(path) => {
                path.compute_output_shape(input_shape, dollar_shape, named_var_shapes, source_id)
            }

            LitExpr::LitPath(literal, subpath) => {
                let literal_shape = literal.compute_output_shape(
                    input_shape,
                    dollar_shape.clone(),
                    named_var_shapes,
                    source_id,
                );
                subpath.compute_output_shape(
                    literal_shape,
                    dollar_shape,
                    named_var_shapes,
                    source_id,
                )
            }
        }
    }
}

impl ApplyToInternal for SubSelection {
    fn apply_to_path(
        &self,
        data: &JSON,
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
    ) -> (Option<JSON>, Vec<ApplyToError>) {
        if let JSON::Array(array) = data {
            return self.apply_to_array(array, vars, input_path);
        }

        let vars: VarsWithPathsMap = {
            let mut vars = vars.clone();
            vars.insert(KnownVariable::Dollar, (data, input_path.clone()));
            vars
        };

        let mut output = JSON::Object(JSONMap::new());
        let mut errors = Vec::new();

        for named_selection in self.selections.iter() {
            let (named_output_opt, apply_errors) =
                named_selection.apply_to_path(data, &vars, input_path);
            errors.extend(apply_errors);

            let (merged, merge_errors) = json_merge(Some(&output), named_output_opt.as_ref());

            errors.extend(
                merge_errors
                    .into_iter()
                    .map(|message| ApplyToError::new(message, input_path.to_vec(), self.range())),
            );

            if let Some(merged) = merged {
                output = merged;
            }
        }

        if !matches!(data, JSON::Object(_)) {
            let output_is_empty = match &output {
                JSON::Object(map) => map.is_empty(),
                _ => false,
            };
            if output_is_empty {
                // If data was a primitive value (neither array nor object), and
                // no output properties were generated, return data as is, along
                // with any errors that occurred.
                return (Some(data.clone()), errors);
            }
        }

        (Some(output), errors)
    }

    fn compute_output_shape(
        &self,
        input_shape: Shape,
        _previous_dollar_shape: Shape,
        named_var_shapes: &IndexMap<&str, Shape>,
        source_id: &SourceId,
    ) -> Shape {
        // Just as SubSelection::apply_to_path calls apply_to_array when data is
        // an array, so compute_output_shape recursively computes the output
        // shapes of each array element shape.
        if let ShapeCase::Array { prefix, tail } = input_shape.case() {
            let new_prefix = prefix
                .iter()
                .map(|shape| {
                    self.compute_output_shape(
                        shape.clone(),
                        shape.clone(),
                        named_var_shapes,
                        source_id,
                    )
                })
                .collect::<Vec<_>>();

            let new_tail = if tail.is_none() {
                tail.clone()
            } else {
                self.compute_output_shape(tail.clone(), tail.clone(), named_var_shapes, source_id)
            };

            return Shape::array(new_prefix, new_tail, self.shape_location(source_id));
        }

        // If the input shape is a named shape, it might end up being an array,
        // so we need to hedge the output shape using a wildcard that maps over
        // array elements.
        let input_shape = input_shape.any_item(Vec::new());

        // The SubSelection rebinds the $ variable to the selected input object,
        // so we can ignore _previous_dollar_shape.
        #[expect(clippy::redundant_clone)]
        let dollar_shape = input_shape.clone();

        // Build up the merged object shape using Shape::all to merge the
        // individual named_selection object shapes.
        let mut all_shape = Shape::empty_object(self.shape_location(source_id));

        for named_selection in self.selections.iter() {
            // Simplifying as we go with Shape::all keeps all_shape relatively
            // small in the common case when all named_selection items return an
            // object shape, since those object shapes can all be merged
            // together into one object.
            all_shape = Shape::all(
                [
                    all_shape,
                    named_selection.compute_output_shape(
                        input_shape.clone(),
                        dollar_shape.clone(),
                        named_var_shapes,
                        source_id,
                    ),
                ],
                self.shape_location(source_id),
            );

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

/// Helper to get the field from a shape or error if the object doesn't have that field.
fn field(shape: &Shape, key: &WithRange<Key>, source_id: &SourceId) -> Shape {
    if let ShapeCase::One(inner) = shape.case() {
        let mut new_fields = Vec::new();
        for inner_field in inner {
            new_fields.push(field(inner_field, key, source_id));
        }
        return Shape::one(new_fields, shape.locations.clone());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::json_selection::PrettyPrintable;
    use crate::selection;

    #[test]
    fn test_apply_to_selection() {
        let data = json!({
            "hello": "world",
            "nested": {
                "hello": "world",
                "world": "hello",
            },
            "array": [
                { "hello": "world 0" },
                { "hello": "world 1" },
                { "hello": "world 2" },
            ],
        });

        let check_ok = |selection: JSONSelection, expected_json: JSON| {
            let (actual_json, errors) = selection.apply_to(&data);
            assert_eq!(actual_json, Some(expected_json));
            assert_eq!(errors, vec![]);
        };

        check_ok(selection!("hello"), json!({"hello": "world"}));

        check_ok(
            selection!("nested"),
            json!({
                "nested": {
                    "hello": "world",
                    "world": "hello",
                },
            }),
        );

        check_ok(selection!("nested.hello"), json!("world"));
        check_ok(selection!("$.nested.hello"), json!("world"));

        check_ok(selection!("nested.world"), json!("hello"));
        check_ok(selection!("$.nested.world"), json!("hello"));

        check_ok(
            selection!("nested hello"),
            json!({
                "hello": "world",
                "nested": {
                    "hello": "world",
                    "world": "hello",
                },
            }),
        );

        check_ok(
            selection!("array { hello }"),
            json!({
                "array": [
                    { "hello": "world 0" },
                    { "hello": "world 1" },
                    { "hello": "world 2" },
                ],
            }),
        );

        check_ok(
            selection!("greetings: array { hello }"),
            json!({
                "greetings": [
                    { "hello": "world 0" },
                    { "hello": "world 1" },
                    { "hello": "world 2" },
                ],
            }),
        );

        check_ok(
            selection!("$.array { hello }"),
            json!([
                { "hello": "world 0" },
                { "hello": "world 1" },
                { "hello": "world 2" },
            ]),
        );

        check_ok(
            selection!("worlds: array.hello"),
            json!({
                "worlds": [
                    "world 0",
                    "world 1",
                    "world 2",
                ],
            }),
        );

        check_ok(
            selection!("worlds: $.array.hello"),
            json!({
                "worlds": [
                    "world 0",
                    "world 1",
                    "world 2",
                ],
            }),
        );

        check_ok(
            selection!("array.hello"),
            json!(["world 0", "world 1", "world 2",]),
        );

        check_ok(
            selection!("$.array.hello"),
            json!(["world 0", "world 1", "world 2",]),
        );

        check_ok(
            selection!("nested grouped: { hello worlds: array.hello }"),
            json!({
                "nested": {
                    "hello": "world",
                    "world": "hello",
                },
                "grouped": {
                    "hello": "world",
                    "worlds": [
                        "world 0",
                        "world 1",
                        "world 2",
                    ],
                },
            }),
        );

        check_ok(
            selection!("nested grouped: { hello worlds: $.array.hello }"),
            json!({
                "nested": {
                    "hello": "world",
                    "world": "hello",
                },
                "grouped": {
                    "hello": "world",
                    "worlds": [
                        "world 0",
                        "world 1",
                        "world 2",
                    ],
                },
            }),
        );
    }

    #[test]
    fn test_apply_to_errors() {
        let data = json!({
            "hello": "world",
            "nested": {
                "hello": 123,
                "world": true,
            },
            "array": [
                { "hello": 1, "goodbye": "farewell" },
                { "hello": "two" },
                { "hello": 3.0, "smello": "yellow" },
            ],
        });

        assert_eq!(
            selection!("hello").apply_to(&data),
            (Some(json!({"hello": "world"})), vec![],)
        );

        fn make_yellow_errors_expected(yellow_range: std::ops::Range<usize>) -> Vec<ApplyToError> {
            vec![ApplyToError::new(
                "Property .yellow not found in object".to_string(),
                vec![json!("yellow")],
                Some(yellow_range),
            )]
        }
        assert_eq!(
            selection!("yellow").apply_to(&data),
            (Some(json!({})), make_yellow_errors_expected(0..6)),
        );
        assert_eq!(
            selection!("$.yellow").apply_to(&data),
            (None, make_yellow_errors_expected(2..8)),
        );

        assert_eq!(
            selection!("nested.hello").apply_to(&data),
            (Some(json!(123)), vec![],)
        );

        fn make_quoted_yellow_expected(
            yellow_range: std::ops::Range<usize>,
        ) -> (Option<JSON>, Vec<ApplyToError>) {
            (
                None,
                vec![ApplyToError::new(
                    "Property .\"yellow\" not found in object".to_string(),
                    vec![json!("nested"), json!("yellow")],
                    Some(yellow_range),
                )],
            )
        }
        assert_eq!(
            selection!("nested.'yellow'").apply_to(&data),
            make_quoted_yellow_expected(7..15),
        );
        assert_eq!(
            selection!("nested.\"yellow\"").apply_to(&data),
            make_quoted_yellow_expected(7..15),
        );
        assert_eq!(
            selection!("$.nested.'yellow'").apply_to(&data),
            make_quoted_yellow_expected(9..17),
        );

        fn make_nested_path_expected(
            hola_range: (usize, usize),
            yellow_range: (usize, usize),
        ) -> (Option<JSON>, Vec<ApplyToError>) {
            (
                Some(json!({
                    "world": true,
                })),
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Property .hola not found in object",
                        "path": ["nested", "hola"],
                        "range": hola_range,
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .yellow not found in object",
                        "path": ["nested", "yellow"],
                        "range": yellow_range,
                    })),
                ],
            )
        }
        assert_eq!(
            selection!("$.nested { hola yellow world }").apply_to(&data),
            make_nested_path_expected((11, 15), (16, 22)),
        );
        assert_eq!(
            selection!(" $ . nested { hola yellow world } ").apply_to(&data),
            make_nested_path_expected((14, 18), (19, 25)),
        );

        fn make_partial_array_expected(
            goodbye_range: (usize, usize),
        ) -> (Option<JSON>, Vec<ApplyToError>) {
            (
                Some(json!({
                    "partial": [
                        { "hello": 1, "goodbye": "farewell" },
                        { "hello": "two" },
                        { "hello": 3.0 },
                    ],
                })),
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Property .goodbye not found in object",
                        "path": ["array", 1, "goodbye"],
                        "range": goodbye_range,
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .goodbye not found in object",
                        "path": ["array", 2, "goodbye"],
                        "range": goodbye_range,
                    })),
                ],
            )
        }
        assert_eq!(
            selection!("partial: $.array { hello goodbye }").apply_to(&data),
            make_partial_array_expected((25, 32)),
        );
        assert_eq!(
            selection!(" partial : $ . array { hello goodbye } ").apply_to(&data),
            make_partial_array_expected((29, 36)),
        );

        assert_eq!(
            selection!("good: array.hello bad: array.smello").apply_to(&data),
            (
                Some(json!({
                    "good": [
                        1,
                        "two",
                        3.0,
                    ],
                    "bad": [
                        null,
                        null,
                        "yellow",
                    ],
                })),
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Property .smello not found in object",
                        "path": ["array", 0, "smello"],
                        "range": [29, 35],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .smello not found in object",
                        "path": ["array", 1, "smello"],
                        "range": [29, 35],
                    })),
                ],
            )
        );

        assert_eq!(
            selection!("array { hello smello }").apply_to(&data),
            (
                Some(json!({
                    "array": [
                        { "hello": 1 },
                        { "hello": "two" },
                        { "hello": 3.0, "smello": "yellow" },
                    ],
                })),
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Property .smello not found in object",
                        "path": ["array", 0, "smello"],
                        "range": [14, 20],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .smello not found in object",
                        "path": ["array", 1, "smello"],
                        "range": [14, 20],
                    })),
                ],
            )
        );

        assert_eq!(
            selection!("$.nested { grouped: { hello smelly world } }").apply_to(&data),
            (
                Some(json!({
                    "grouped": {
                        "hello": 123,
                        "world": true,
                    },
                })),
                vec![ApplyToError::from_json(&json!({
                    "message": "Property .smelly not found in object",
                    "path": ["nested", "smelly"],
                    "range": [28, 34],
                })),],
            )
        );

        assert_eq!(
            selection!("alias: $.nested { grouped: { hello smelly world } }").apply_to(&data),
            (
                Some(json!({
                    "alias": {
                        "grouped": {
                            "hello": 123,
                            "world": true,
                        },
                    },
                })),
                vec![ApplyToError::from_json(&json!({
                    "message": "Property .smelly not found in object",
                    "path": ["nested", "smelly"],
                    "range": [35, 41],
                }))],
            )
        );
    }

    #[test]
    fn test_apply_to_nested_arrays() {
        let data = json!({
            "arrayOfArrays": [
                [
                    { "x": 0, "y": 0 },
                ],
                [
                    { "x": 1, "y": 0 },
                    { "x": 1, "y": 1 },
                    { "x": 1, "y": 2 },
                ],
                [
                    { "x": 2, "y": 0 },
                    { "x": 2, "y": 1 },
                ],
                [],
                [
                    null,
                    { "x": 4, "y": 1 },
                    { "x": 4, "why": 2 },
                    null,
                    { "x": 4, "y": 4 },
                ]
            ],
        });

        fn make_array_of_arrays_x_expected(
            x_range: (usize, usize),
        ) -> (Option<JSON>, Vec<ApplyToError>) {
            (
                Some(json!([[0], [1, 1, 1], [2, 2], [], [null, 4, 4, null, 4]])),
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Property .x not found in null",
                        "path": ["arrayOfArrays", 4, 0, "x"],
                        "range": x_range,
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .x not found in null",
                        "path": ["arrayOfArrays", 4, 3, "x"],
                        "range": x_range,
                    })),
                ],
            )
        }
        assert_eq!(
            selection!("arrayOfArrays.x").apply_to(&data),
            make_array_of_arrays_x_expected((14, 15)),
        );
        assert_eq!(
            selection!("$.arrayOfArrays.x").apply_to(&data),
            make_array_of_arrays_x_expected((16, 17)),
        );

        fn make_array_of_arrays_y_expected(
            y_range: (usize, usize),
        ) -> (Option<JSON>, Vec<ApplyToError>) {
            (
                Some(json!([
                    [0],
                    [0, 1, 2],
                    [0, 1],
                    [],
                    [null, 1, null, null, 4],
                ])),
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Property .y not found in null",
                        "path": ["arrayOfArrays", 4, 0, "y"],
                        "range": y_range,
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .y not found in object",
                        "path": ["arrayOfArrays", 4, 2, "y"],
                        "range": y_range,
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .y not found in null",
                        "path": ["arrayOfArrays", 4, 3, "y"],
                        "range": y_range,
                    })),
                ],
            )
        }
        assert_eq!(
            selection!("arrayOfArrays.y").apply_to(&data),
            make_array_of_arrays_y_expected((14, 15)),
        );
        assert_eq!(
            selection!("$.arrayOfArrays.y").apply_to(&data),
            make_array_of_arrays_y_expected((16, 17)),
        );

        assert_eq!(
            selection!("alias: arrayOfArrays { x y }").apply_to(&data),
            (
                Some(json!({
                    "alias": [
                        [
                            { "x": 0, "y": 0 },
                        ],
                        [
                            { "x": 1, "y": 0 },
                            { "x": 1, "y": 1 },
                            { "x": 1, "y": 2 },
                        ],
                        [
                            { "x": 2, "y": 0 },
                            { "x": 2, "y": 1 },
                        ],
                        [],
                        [
                            null,
                            { "x": 4, "y": 1 },
                            { "x": 4 },
                            null,
                            { "x": 4, "y": 4 },
                        ]
                    ],
                })),
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Property .x not found in null",
                        "path": ["arrayOfArrays", 4, 0, "x"],
                        "range": [23, 24],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .y not found in null",
                        "path": ["arrayOfArrays", 4, 0, "y"],
                        "range": [25, 26],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .y not found in object",
                        "path": ["arrayOfArrays", 4, 2, "y"],
                        "range": [25, 26],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .x not found in null",
                        "path": ["arrayOfArrays", 4, 3, "x"],
                        "range": [23, 24],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .y not found in null",
                        "path": ["arrayOfArrays", 4, 3, "y"],
                        "range": [25, 26],
                    })),
                ],
            ),
        );

        fn make_array_of_arrays_x_y_expected(
            x_range: (usize, usize),
            y_range: (usize, usize),
        ) -> (Option<JSON>, Vec<ApplyToError>) {
            (
                Some(json!({
                    "ys": [
                        [0],
                        [0, 1, 2],
                        [0, 1],
                        [],
                        [null, 1, null, null, 4],
                    ],
                    "xs": [
                        [0],
                        [1, 1, 1],
                        [2, 2],
                        [],
                        [null, 4, 4, null, 4],
                    ],
                })),
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Property .y not found in null",
                        "path": ["arrayOfArrays", 4, 0, "y"],
                        "range": y_range,
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .y not found in object",
                        "path": ["arrayOfArrays", 4, 2, "y"],
                        "range": y_range,
                    })),
                    ApplyToError::from_json(&json!({
                        // Reversing the order of "path" and "message" here to make
                        // sure that doesn't affect the deduplication logic.
                        "path": ["arrayOfArrays", 4, 3, "y"],
                        "message": "Property .y not found in null",
                        "range": y_range,
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .x not found in null",
                        "path": ["arrayOfArrays", 4, 0, "x"],
                        "range": x_range,
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .x not found in null",
                        "path": ["arrayOfArrays", 4, 3, "x"],
                        "range": x_range,
                    })),
                ],
            )
        }
        assert_eq!(
            selection!("ys: arrayOfArrays.y xs: arrayOfArrays.x").apply_to(&data),
            make_array_of_arrays_x_y_expected((38, 39), (18, 19)),
        );
        assert_eq!(
            selection!("ys: $.arrayOfArrays.y xs: $.arrayOfArrays.x").apply_to(&data),
            make_array_of_arrays_x_y_expected((42, 43), (20, 21)),
        );
    }

    #[test]
    fn test_apply_to_variable_expressions() {
        let id_object = selection!("id: $").apply_to(&json!(123));
        assert_eq!(id_object, (Some(json!({"id": 123})), vec![]));

        let data = json!({
            "id": 123,
            "name": "Ben",
            "friend_ids": [234, 345, 456]
        });

        assert_eq!(
            selection!("id name friends: friend_ids { id: $ }").apply_to(&data),
            (
                Some(json!({
                    "id": 123,
                    "name": "Ben",
                    "friends": [
                        { "id": 234 },
                        { "id": 345 },
                        { "id": 456 },
                    ],
                })),
                vec![],
            ),
        );

        let mut vars = IndexMap::default();
        vars.insert("$args".to_string(), json!({ "id": "id from args" }));
        assert_eq!(
            selection!("id: $args.id name").apply_with_vars(&data, &vars),
            (
                Some(json!({
                    "id": "id from args",
                    "name": "Ben"
                })),
                vec![],
            ),
        );
        assert_eq!(
            selection!("nested.path { id: $args.id name }").apply_to(&json!({
                "nested": {
                    "path": data,
                },
            })),
            (
                Some(json!({
                    "name": "Ben"
                })),
                vec![ApplyToError::from_json(&json!({
                    "message": "Variable $args not found",
                    "path": ["nested", "path"],
                    "range": [18, 23],
                }))],
            ),
        );
        let mut vars_without_args_id = IndexMap::default();
        vars_without_args_id.insert("$args".to_string(), json!({ "unused": "ignored" }));
        assert_eq!(
            selection!("id: $args.id name").apply_with_vars(&data, &vars_without_args_id),
            (
                Some(json!({
                    "name": "Ben"
                })),
                vec![ApplyToError::from_json(&json!({
                    "message": "Property .id not found in object",
                    "path": ["$args", "id"],
                    "range": [10, 12],
                }))],
            ),
        );

        // A single variable path should not be mapped over an input array.
        assert_eq!(
            selection!("$args.id").apply_with_vars(&json!([1, 2, 3]), &vars),
            (Some(json!("id from args")), vec![]),
        );
    }

    #[test]
    fn test_apply_to_variable_expressions_typename() {
        let typename_object =
            selection!("__typename: $->echo('Product') reviews { __typename: $->echo('Review') }")
                .apply_to(&json!({"reviews": [{}]}));
        assert_eq!(
            typename_object,
            (
                Some(json!({"__typename": "Product", "reviews": [{ "__typename": "Review" }] })),
                vec![]
            )
        );
    }

    #[test]
    fn test_literal_expressions_in_parentheses() {
        assert_eq!(
            selection!("__typename: $('Product')").apply_to(&json!({})),
            (Some(json!({"__typename": "Product"})), vec![]),
        );

        assert_eq!(
            selection!(" __typename : 'Product' ").apply_to(&json!({})),
            (
                Some(json!({})),
                vec![ApplyToError::new(
                    "Property .\"Product\" not found in object".to_string(),
                    vec![json!("Product")],
                    Some(14..23),
                )],
            ),
        );

        assert_eq!(
            selection!(
                r#"
                one: $(1)
                two: $(2)
                negativeThree: $(-  3)
                true: $(true  )
                false: $(  false)
                null: $(null)
                string: $("string")
                array: $( [ 1 , 2 , 3 ] )
                object: $( { "key" : "value" } )
                path: $(nested.path)
            "#
            )
            .apply_to(&json!({
                "nested": {
                    "path": "nested path value"
                }
            })),
            (
                Some(json!({
                    "one": 1,
                    "two": 2,
                    "negativeThree": -3,
                    "true": true,
                    "false": false,
                    "null": null,
                    "string": "string",
                    "array": [1, 2, 3],
                    "object": { "key": "value" },
                    "path": "nested path value",
                })),
                vec![],
            ),
        );

        assert_eq!(
            selection!(
                r#"
                one: $(1)->typeof
                two: $(2)->typeof
                negativeThree: $(-3)->typeof
                true: $(true)->typeof
                false: $(false)->typeof
                null: $(null)->typeof
                string: $("string")->typeof
                array: $([1, 2, 3])->typeof
                object: $({ "key": "value" })->typeof
                path: $(nested.path)->typeof
            "#
            )
            .apply_to(&json!({
                "nested": {
                    "path": 12345
                }
            })),
            (
                Some(json!({
                    "one": "number",
                    "two": "number",
                    "negativeThree": "number",
                    "true": "boolean",
                    "false": "boolean",
                    "null": "null",
                    "string": "string",
                    "array": "array",
                    "object": "object",
                    "path": "number",
                })),
                vec![],
            ),
        );

        assert_eq!(
            selection!(
                r#"
                items: $([
                    1,
                    -2.0,
                    true,
                    false,
                    null,
                    "string",
                    [1, 2, 3],
                    { "key": "value" },
                    nested.path,
                ])->map(@->typeof)
            "#
            )
            .apply_to(&json!({
                "nested": {
                    "path": { "deeply": "nested" }
                }
            })),
            (
                Some(json!({
                    "items": [
                        "number",
                        "number",
                        "boolean",
                        "boolean",
                        "null",
                        "string",
                        "array",
                        "object",
                        "object",
                    ],
                })),
                vec![],
            ),
        );

        assert_eq!(
            selection!(
                r#"
                $({
                    one: 1,
                    two: 2,
                    negativeThree: -3,
                    true: true,
                    false: false,
                    null: null,
                    string: "string",
                    array: [1, 2, 3],
                    object: { "key": "value" },
                    path: $ . nested . path ,
                })->entries
            "#
            )
            .apply_to(&json!({
                "nested": {
                    "path": "nested path value"
                }
            })),
            (
                Some(json!([
                    { "key": "one", "value": 1 },
                    { "key": "two", "value": 2 },
                    { "key": "negativeThree", "value": -3 },
                    { "key": "true", "value": true },
                    { "key": "false", "value": false },
                    { "key": "null", "value": null },
                    { "key": "string", "value": "string" },
                    { "key": "array", "value": [1, 2, 3] },
                    { "key": "object", "value": { "key": "value" } },
                    { "key": "path", "value": "nested path value" },
                ])),
                vec![],
            ),
        );

        assert_eq!(
            selection!(
                r#"
                $({
                    string: $("string")->slice(1, 4),
                    array: $([1, 2, 3])->map(@->add(10)),
                    object: $({ "key": "value" })->get("key"),
                    path: nested.path->slice($("nested ")->size),
                    needlessParens: $("oyez"),
                    withoutParens: "oyez",
                })
            "#
            )
            .apply_to(&json!({
                "nested": {
                    "path": "nested path value"
                }
            })),
            (
                Some(json!({
                    "string": "tri",
                    "array": [11, 12, 13],
                    "object": "value",
                    "path": "path value",
                    "needlessParens": "oyez",
                    "withoutParens": "oyez",
                })),
                vec![],
            ),
        );

        assert_eq!(
            selection!(
                r#"
                string: $("string")->slice(1, 4)
                array: $([1, 2, 3])->map(@->add(10))
                object: $({ "key": "value" })->get("key")
                path: nested.path->slice($("nested ")->size)
            "#
            )
            .apply_to(&json!({
                "nested": {
                    "path": "nested path value"
                }
            })),
            (
                Some(json!({
                    "string": "tri",
                    "array": [11, 12, 13],
                    "object": "value",
                    "path": "path value",
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn test_inline_paths_with_subselections() {
        let data = json!({
            "id": 123,
            "created": "2021-01-01T00:00:00Z",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "The capital of Australia is Canberra.",
                },
            }, {
                "index": 1,
                "message": {
                    "role": "assistant",
                    "content": "The capital of Australia is Sydney.",
                },
            }],
        });

        {
            let expected = (
                Some(json!({
                    "id": 123,
                    "created": "2021-01-01T00:00:00Z",
                    "model": "gpt-4o",
                    "role": "assistant",
                    "content": "The capital of Australia is Canberra.",
                })),
                vec![],
            );

            assert_eq!(
                selection!(
                    r#"
                    id
                    created
                    model
                    role: choices->first.message.role
                    content: choices->first.message.content
                "#
                )
                .apply_to(&data),
                expected,
            );

            assert_eq!(
                selection!(
                    r#"
                    id
                    created
                    model
                    choices->first.message {
                        role
                        content
                    }
                "#
                )
                .apply_to(&data),
                expected,
            );

            assert_eq!(
                selection!(
                    r#"
                    id
                    choices->first.message {
                        role
                        content
                    }
                    created
                    model
                "#
                )
                .apply_to(&data),
                expected,
            );
        }

        {
            let expected = (
                Some(json!({
                    "id": 123,
                    "created": "2021-01-01T00:00:00Z",
                    "model": "gpt-4o",
                    "role": "assistant",
                    "message": "The capital of Australia is Sydney.",
                })),
                vec![],
            );

            assert_eq!(
                selection!(
                    r#"
                    id
                    created
                    model
                    role: choices->last.message.role
                    message: choices->last.message.content
                "#
                )
                .apply_to(&data),
                expected,
            );

            assert_eq!(
                selection!(
                    r#"
                    id
                    created
                    model
                    choices->last.message {
                        role
                        message: content
                    }
                "#
                )
                .apply_to(&data),
                expected,
            );

            assert_eq!(
                selection!(
                    r#"
                    created
                    choices->last.message {
                        message: content
                        role
                    }
                    model
                    id
                "#
                )
                .apply_to(&data),
                expected,
            );
        }

        {
            let expected = (
                Some(json!({
                    "id": 123,
                    "created": "2021-01-01T00:00:00Z",
                    "model": "gpt-4o",
                    "role": "assistant",
                    "correct": "The capital of Australia is Canberra.",
                    "incorrect": "The capital of Australia is Sydney.",
                })),
                vec![],
            );

            assert_eq!(
                selection!(
                    r#"
                    id
                    created
                    model
                    role: choices->first.message.role
                    correct: choices->first.message.content
                    incorrect: choices->last.message.content
                "#
                )
                .apply_to(&data),
                expected,
            );

            assert_eq!(
                selection!(
                    r#"
                    id
                    created
                    model
                    choices->first.message {
                        role
                        correct: content
                    }
                    choices->last.message {
                        incorrect: content
                    }
                "#
                )
                .apply_to(&data),
                expected,
            );

            assert_eq!(
                selection!(
                    r#"
                    id
                    created
                    model
                    choices->first.message {
                        role
                        correct: content
                    }
                    incorrect: choices->last.message.content
                "#
                )
                .apply_to(&data),
                expected,
            );

            assert_eq!(
                selection!(
                    r#"
                    id
                    created
                    model
                    choices->first.message {
                        correct: content
                    }
                    choices->last.message {
                        role
                        incorrect: content
                    }
                "#
                )
                .apply_to(&data),
                expected,
            );

            assert_eq!(
                selection!(
                    r#"
                    id
                    created
                    correct: choices->first.message.content
                    choices->last.message {
                        role
                        incorrect: content
                    }
                    model
                "#
                )
                .apply_to(&data),
                expected,
            );
        }

        {
            let data = json!({
                "from": "data",
            });

            let vars = {
                let mut vars = IndexMap::default();
                vars.insert(
                    "$this".to_string(),
                    json!({
                        "id": 1234,
                    }),
                );
                vars.insert(
                    "$args".to_string(),
                    json!({
                        "input": {
                            "title": "The capital of Australia",
                            "body": "Canberra",
                        },
                        "extra": "extra",
                    }),
                );
                vars
            };

            let expected = (
                Some(json!({
                    "id": 1234,
                    "title": "The capital of Australia",
                    "body": "Canberra",
                    "from": "data",
                })),
                vec![],
            );

            assert_eq!(
                selection!(
                    r#"
                    id: $this.id
                    $args.input {
                        title
                        body
                    }
                    from
                "#
                )
                .apply_with_vars(&data, &vars),
                expected,
            );

            assert_eq!(
                selection!(
                    r#"
                    from
                    $args.input { title body }
                    id: $this.id
                "#
                )
                .apply_with_vars(&data, &vars),
                expected,
            );

            assert_eq!(
                selection!(
                    r#"
                    $args.input { body title }
                    from
                    id: $this.id
                "#
                )
                .apply_with_vars(&data, &vars),
                expected,
            );

            assert_eq!(
                selection!(
                    r#"
                    id: $this.id
                    $args { $.input { title body } }
                    from
                "#
                )
                .apply_with_vars(&data, &vars),
                expected,
            );

            assert_eq!(
                selection!(
                    r#"
                    id: $this.id
                    $args { $.input { title body } extra }
                    from: $.from
                "#
                )
                .apply_with_vars(&data, &vars),
                (
                    Some(json!({
                        "id": 1234,
                        "title": "The capital of Australia",
                        "body": "Canberra",
                        "extra": "extra",
                        "from": "data",
                    })),
                    vec![],
                ),
            );

            assert_eq!(
                selection!(
                    r#"
                    # Equivalent to id: $this.id
                    $this { id }

                    $args {
                        __typename: $("Args")

                        # Requiring $. instead of just . prevents .input from
                        # parsing as a key applied to the $("Args") string.
                        $.input { title body }

                        extra
                    }

                    from: $.from
                "#
                )
                .apply_with_vars(&data, &vars),
                (
                    Some(json!({
                        "id": 1234,
                        "title": "The capital of Australia",
                        "body": "Canberra",
                        "__typename": "Args",
                        "extra": "extra",
                        "from": "data",
                    })),
                    vec![],
                ),
            );
        }
    }

    #[test]
    fn test_inline_path_errors() {
        {
            let data = json!({
                "id": 123,
                "created": "2021-01-01T00:00:00Z",
                "model": "gpt-4o",
                "choices": [{
                    "message": "The capital of Australia is Canberra.",
                }, {
                    "message": "The capital of Australia is Sydney.",
                }],
            });

            let expected = (
                Some(json!({
                    "id": 123,
                    "created": "2021-01-01T00:00:00Z",
                    "model": "gpt-4o",
                })),
                vec![
                    ApplyToError::new(
                        "Property .role not found in string".to_string(),
                        vec![
                            json!("choices"),
                            json!("->first"),
                            json!("message"),
                            json!("role"),
                        ],
                        Some(123..127),
                    ),
                    ApplyToError::new(
                        "Property .content not found in string".to_string(),
                        vec![
                            json!("choices"),
                            json!("->first"),
                            json!("message"),
                            json!("content"),
                        ],
                        Some(128..135),
                    ),
                    ApplyToError::new(
                        "Expected object or null, not string".to_string(),
                        vec![],
                        // This is the range of the whole
                        // `choices->first.message { role content }`
                        // subselection.
                        Some(98..137),
                    ),
                ],
            );

            assert_eq!(
                selection!(
                    r#"
                    id
                    created
                    model
                    choices->first.message { role content }
                "#
                )
                .apply_to(&data),
                expected,
            );
        }

        assert_eq!(
            selection!("id nested.path.nonexistent { name }").apply_to(&json!({
                "id": 2345,
                "nested": {
                    "path": "nested path value",
                },
            })),
            (
                Some(json!({
                    "id": 2345,
                })),
                vec![
                    ApplyToError::new(
                        "Property .nonexistent not found in string".to_string(),
                        vec![json!("nested"), json!("path"), json!("nonexistent")],
                        Some(15..26),
                    ),
                    ApplyToError::new(
                        "Expected object or null, not nothing".to_string(),
                        vec![],
                        // This is the range of the whole
                        // `nested.path.nonexistent { name }` path selection.
                        Some(3..35),
                    ),
                ],
            ),
        );

        // We have to construct this invalid selection manually because we want
        // to test an error case requiring a PathWithSubSelection that does not
        // actually have a SubSelection, which should not be possible to
        // construct through normal parsing.
        let invalid_inline_path_selection = JSONSelection::Named(SubSelection {
            selections: vec![NamedSelection::Path {
                alias: None,
                inline: false,
                path: PathSelection {
                    path: PathList::Key(
                        Key::field("some").into_with_range(),
                        PathList::Key(
                            Key::field("number").into_with_range(),
                            PathList::Empty.into_with_range(),
                        )
                        .into_with_range(),
                    )
                    .into_with_range(),
                },
            }],
            ..Default::default()
        });

        assert_eq!(
            invalid_inline_path_selection.apply_to(&json!({
                "some": {
                    "number": 579,
                },
            })),
            (
                Some(json!({})),
                vec![ApplyToError::new(
                    "Named path must have an alias, a trailing subselection, or be inlined with ... and produce an object or null".to_string(),
                    vec![],
                    // No range because this is a manually constructed selection.
                    None,
                ),],
            ),
        );

        let valid_inline_path_selection = JSONSelection::Named(SubSelection {
            selections: vec![NamedSelection::Path {
                alias: None,
                inline: true, // This makes it valid.
                path: PathSelection {
                    path: PathList::Key(
                        Key::field("some").into_with_range(),
                        PathList::Key(
                            Key::field("object").into_with_range(),
                            PathList::Empty.into_with_range(),
                        )
                        .into_with_range(),
                    )
                    .into_with_range(),
                },
            }],
            ..Default::default()
        });

        assert_eq!(
            valid_inline_path_selection.apply_to(&json!({
                "some": {
                    "object": {
                        "key": "value",
                    },
                },
            })),
            (
                Some(json!({
                    "key": "value",
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn test_apply_to_non_identifier_properties() {
        let data = json!({
            "not an identifier": [
                { "also.not.an.identifier": 0 },
                { "also.not.an.identifier": 1 },
                { "also.not.an.identifier": 2 },
            ],
            "another": {
                "pesky string literal!": {
                    "identifier": 123,
                    "{ evil braces }": true,
                },
            },
        });

        assert_eq!(
            // The grammar enforces that we must always provide identifier aliases
            // for non-identifier properties, so the data we get back will always be
            // GraphQL-safe.
            selection!("alias: 'not an identifier' { safe: 'also.not.an.identifier' }")
                .apply_to(&data),
            (
                Some(json!({
                    "alias": [
                        { "safe": 0 },
                        { "safe": 1 },
                        { "safe": 2 },
                    ],
                })),
                vec![],
            ),
        );

        assert_eq!(
            selection!("'not an identifier'.'also.not.an.identifier'").apply_to(&data),
            (Some(json!([0, 1, 2])), vec![],),
        );

        assert_eq!(
            selection!("$.'not an identifier'.'also.not.an.identifier'").apply_to(&data),
            (Some(json!([0, 1, 2])), vec![],),
        );

        assert_eq!(
            selection!("$.\"not an identifier\" { safe: \"also.not.an.identifier\" }")
                .apply_to(&data),
            (
                Some(json!([
                    { "safe": 0 },
                    { "safe": 1 },
                    { "safe": 2 },
                ])),
                vec![],
            ),
        );

        assert_eq!(
            selection!(
                "another {
                pesky: 'pesky string literal!' {
                    identifier
                    evil: '{ evil braces }'
                }
            }"
            )
            .apply_to(&data),
            (
                Some(json!({
                    "another": {
                        "pesky": {
                            "identifier": 123,
                            "evil": true,
                        },
                    },
                })),
                vec![],
            ),
        );

        assert_eq!(
            selection!("another.'pesky string literal!'.'{ evil braces }'").apply_to(&data),
            (Some(json!(true)), vec![],),
        );

        assert_eq!(
            selection!("another.'pesky string literal!'.\"identifier\"").apply_to(&data),
            (Some(json!(123)), vec![],),
        );

        assert_eq!(
            selection!("$.another.'pesky string literal!'.\"identifier\"").apply_to(&data),
            (Some(json!(123)), vec![],),
        );
    }

    #[test]
    fn test_left_associative_path_evaluation() {
        assert_eq!(
            selection!("batch.id->first").apply_to(&json!({
                "batch": [
                    { "id": 1 },
                    { "id": 2 },
                    { "id": 3 },
                ],
            })),
            (Some(json!(1)), vec![]),
        );

        assert_eq!(
            selection!("batch.id->last").apply_to(&json!({
                "batch": [
                    { "id": 1 },
                    { "id": 2 },
                    { "id": 3 },
                ],
            })),
            (Some(json!(3)), vec![]),
        );

        assert_eq!(
            selection!("batch.id->size").apply_to(&json!({
                "batch": [
                    { "id": 1 },
                    { "id": 2 },
                    { "id": 3 },
                ],
            })),
            (Some(json!(3)), vec![]),
        );

        assert_eq!(
            selection!("batch.id->slice(1)->first").apply_to(&json!({
                "batch": [
                    { "id": 1 },
                    { "id": 2 },
                    { "id": 3 },
                ],
            })),
            (Some(json!(2)), vec![]),
        );

        assert_eq!(
            selection!("batch.id->map({ batchId: @ })").apply_to(&json!({
                "batch": [
                    { "id": 1 },
                    { "id": 2 },
                    { "id": 3 },
                ],
            })),
            (
                Some(json!([
                    { "batchId": 1 },
                    { "batchId": 2 },
                    { "batchId": 3 },
                ])),
                vec![],
            ),
        );

        let mut vars = IndexMap::default();
        vars.insert(
            "$batch".to_string(),
            json!([
                { "id": 4 },
                { "id": 5 },
                { "id": 6 },
            ]),
        );
        assert_eq!(
            selection!("$batch.id->map({ batchId: @ })").apply_with_vars(
                &json!({
                    "batch": "ignored",
                }),
                &vars
            ),
            (
                Some(json!([
                    { "batchId": 4 },
                    { "batchId": 5 },
                    { "batchId": 6 },
                ])),
                vec![],
            ),
        );

        assert_eq!(
            selection!("batch.id->map({ batchId: @ })->first").apply_to(&json!({
                "batch": [
                    { "id": 7 },
                    { "id": 8 },
                    { "id": 9 },
                ],
            })),
            (Some(json!({ "batchId": 7 })), vec![]),
        );

        assert_eq!(
            selection!("batch.id->map({ batchId: @ })->last").apply_to(&json!({
                "batch": [
                    { "id": 7 },
                    { "id": 8 },
                    { "id": 9 },
                ],
            })),
            (Some(json!({ "batchId": 9 })), vec![]),
        );

        assert_eq!(
            selection!("$batch.id->map({ batchId: @ })->first").apply_with_vars(
                &json!({
                    "batch": "ignored",
                }),
                &vars
            ),
            (Some(json!({ "batchId": 4 })), vec![]),
        );

        assert_eq!(
            selection!("$batch.id->map({ batchId: @ })->last").apply_with_vars(
                &json!({
                    "batch": "ignored",
                }),
                &vars
            ),
            (Some(json!({ "batchId": 6 })), vec![]),
        );

        assert_eq!(
            selection!("arrays.as.bs->echo({ echoed: @ })").apply_to(&json!({
                "arrays": [
                    { "as": { "bs": [10, 20, 30] } },
                    { "as": { "bs": [40, 50, 60] } },
                    { "as": { "bs": [70, 80, 90] } },
                ],
            })),
            (
                Some(json!({
                    "echoed": [
                        [10, 20, 30],
                        [40, 50, 60],
                        [70, 80, 90],
                    ],
                })),
                vec![],
            ),
        );

        assert_eq!(
            selection!("arrays.as.bs->echo({ echoed: @ })").apply_to(&json!({
                "arrays": [
                    { "as": { "bs": [10, 20, 30] } },
                    { "as": [
                        { "bs": [40, 50, 60] },
                        { "bs": [70, 80, 90] },
                    ] },
                    { "as": { "bs": [100, 110, 120] } },
                ],
            })),
            (
                Some(json!({
                    "echoed": [
                        [10, 20, 30],
                        [
                            [40, 50, 60],
                            [70, 80, 90],
                        ],
                        [100, 110, 120],
                    ],
                })),
                vec![],
            ),
        );

        assert_eq!(
            selection!("batch.id->jsonStringify").apply_to(&json!({
                "batch": [
                    { "id": 1 },
                    { "id": 2 },
                    { "id": 3 },
                ],
            })),
            (Some(json!("[1,2,3]")), vec![]),
        );

        assert_eq!(
            selection!("batch.id->map([@])->echo([@])->jsonStringify").apply_to(&json!({
                "batch": [
                    { "id": 1 },
                    { "id": 2 },
                    { "id": 3 },
                ],
            })),
            (Some(json!("[[[1],[2],[3]]]")), vec![]),
        );

        assert_eq!(
            selection!("batch.id->map([@])->echo([@])->jsonStringify->typeof").apply_to(&json!({
                "batch": [
                    { "id": 1 },
                    { "id": 2 },
                    { "id": 3 },
                ],
            })),
            (Some(json!("string")), vec![]),
        );
    }

    #[test]
    fn test_lit_paths() {
        let data = json!({
            "value": {
                "key": 123,
            },
        });

        assert_eq!(
            selection!("$(\"a\")->first").apply_to(&data),
            (Some(json!("a")), vec![]),
        );

        assert_eq!(
            selection!("$('asdf'->last)").apply_to(&data),
            (Some(json!("f")), vec![]),
        );

        assert_eq!(
            selection!("$(1234)->add(1111)").apply_to(&data),
            (Some(json!(2345)), vec![]),
        );

        assert_eq!(
            selection!("$(1234->add(1111))").apply_to(&data),
            (Some(json!(2345)), vec![]),
        );

        assert_eq!(
            selection!("$(value.key->mul(10))").apply_to(&data),
            (Some(json!(1230)), vec![]),
        );

        assert_eq!(
            selection!("$(value.key)->mul(10)").apply_to(&data),
            (Some(json!(1230)), vec![]),
        );

        assert_eq!(
            selection!("$(value.key->typeof)").apply_to(&data),
            (Some(json!("number")), vec![]),
        );

        assert_eq!(
            selection!("$(value.key)->typeof").apply_to(&data),
            (Some(json!("number")), vec![]),
        );

        assert_eq!(
            selection!("$([1, 2, 3])->last").apply_to(&data),
            (Some(json!(3)), vec![]),
        );

        assert_eq!(
            selection!("$([1, 2, 3]->first)").apply_to(&data),
            (Some(json!(1)), vec![]),
        );

        assert_eq!(
            selection!("$({ a: 'ay', b: 1 }).a").apply_to(&data),
            (Some(json!("ay")), vec![]),
        );

        assert_eq!(
            selection!("$({ a: 'ay', b: 2 }.a)").apply_to(&data),
            (Some(json!("ay")), vec![]),
        );

        assert_eq!(
            // Note that the -> has lower precedence than the -, so -1 is parsed
            // as a completed expression before applying the ->add(10) method,
            // giving 9 instead of -11.
            selection!("$(-1->add(10))").apply_to(&data),
            (Some(json!(9)), vec![]),
        );
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
        //     // $root.data.0.maybe.{this,that} shape references.
        //     //
        //     // We could try to say that any { ... } shape represents either an
        //     // object or a list of objects, by policy, to avoid having to write
        //     // One<{...}, List<{...}>> everywhere a SubSelection appears.
        //     //
        //     // But then we don't know where the array indexes should go...
        //     "{ thisOrThat: One<$root.data.*.maybe.this, $root.data.*.maybe.that> }",
        // );

        assert_eq!(
            selection!(
                r#"
                id
                name
                friends: friend_ids { id: @ }
                alias: arrayOfArrays { x y }
                ys: arrayOfArrays.y xs: arrayOfArrays.x
            "#
            )
            .shape()
            .pretty_print(),
            // This output shape is wrong if $root.friend_ids turns out to be an
            // array, and it's tricky to see how to transform the shape to what
            // it would have been if we knew that, where friends: List<{ id:
            // $root.friend_ids.* }> (note the * meaning any array index),
            // because who's to say it's not the id field that should become the
            // List, rather than the friends field?
            "{ alias: { x: $root.*.arrayOfArrays.*.x, y: $root.*.arrayOfArrays.*.y }, friends: { id: $root.*.friend_ids.* }, id: $root.*.id, name: $root.*.name, xs: $root.*.arrayOfArrays.x, ys: $root.*.arrayOfArrays.y }",
        );

        // TODO: re-test when method type checking is re-enabled
        // assert_eq!(
        //     selection!(r#"
        //         id
        //         name
        //         friends: friend_ids->map({ id: @ })
        //         alias: arrayOfArrays { x y }
        //         ys: arrayOfArrays.y xs: arrayOfArrays.x
        //     "#).shape().pretty_print(),
        //     "{ alias: { x: $root.*.arrayOfArrays.*.x, y: $root.*.arrayOfArrays.*.y }, friends: List<{ id: $root.*.friend_ids.* }>, id: $root.*.id, name: $root.*.name, xs: $root.*.arrayOfArrays.x, ys: $root.*.arrayOfArrays.y }",
        // );
        //
        // assert_eq!(
        //     selection!("$->echo({ thrice: [@, @, @] })")
        //         .shape()
        //         .pretty_print(),
        //     "{ thrice: [$root, $root, $root] }",
        // );
        //
        // assert_eq!(
        //     selection!("$->echo({ thrice: [@, @, @] })->entries")
        //         .shape()
        //         .pretty_print(),
        //     "[{ key: \"thrice\", value: [$root, $root, $root] }]",
        // );
        //
        // assert_eq!(
        //     selection!("$->echo({ thrice: [@, @, @] })->entries.key")
        //         .shape()
        //         .pretty_print(),
        //     "[\"thrice\"]",
        // );
        //
        // assert_eq!(
        //     selection!("$->echo({ thrice: [@, @, @] })->entries.value")
        //         .shape()
        //         .pretty_print(),
        //     "[[$root, $root, $root]]",
        // );
        //
        // assert_eq!(
        //     selection!("$->echo({ wrapped: @ })->entries { k: key v: value }")
        //         .shape()
        //         .pretty_print(),
        //     "[{ k: \"wrapped\", v: $root }]",
        // );
    }

    #[test]
    fn test_optional_key_access_with_existing_property() {
        use serde_json_bytes::json;

        let data = json!({
            "user": {
                "profile": {
                    "name": "Alice"
                }
            }
        });

        let (result, errors) = JSONSelection::parse("$.user?.profile.name")
            .unwrap()
            .apply_to(&data);
        assert!(errors.is_empty());
        assert_eq!(result, Some(json!("Alice")));
    }

    #[test]
    fn test_optional_key_access_with_null_value() {
        use serde_json_bytes::json;

        let data_null = json!({
            "user": null
        });

        let (result, errors) = JSONSelection::parse("$.user?.profile.name")
            .unwrap()
            .apply_to(&data_null);
        assert!(errors.is_empty());
        assert_eq!(result, Some(json!(null)));
    }

    #[test]
    fn test_optional_key_access_on_non_object() {
        use serde_json_bytes::json;

        let data_non_obj = json!({
            "user": "not an object"
        });

        let (result, errors) = JSONSelection::parse("$.user?.profile.name")
            .unwrap()
            .apply_to(&data_non_obj);
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0]
                .message()
                .contains("Property .profile not found in string")
        );
        assert_eq!(result, None);
    }

    #[test]
    fn test_optional_key_access_with_missing_property() {
        use serde_json_bytes::json;

        let data = json!({
            "user": {
                "other": "value"
            }
        });

        let (result, errors) = JSONSelection::parse("$.user?.profile.name")
            .unwrap()
            .apply_to(&data);
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0]
                .message()
                .contains("Property .profile not found in object")
        );
        assert_eq!(result, None);
    }

    #[test]
    fn test_chained_optional_key_access() {
        use serde_json_bytes::json;

        let data = json!({
            "user": {
                "profile": {
                    "name": "Alice"
                }
            }
        });

        let (result, errors) = JSONSelection::parse("$.user?.profile?.name")
            .unwrap()
            .apply_to(&data);
        assert!(errors.is_empty());
        assert_eq!(result, Some(json!("Alice")));
    }

    #[test]
    fn test_chained_optional_access_with_null_in_middle() {
        use serde_json_bytes::json;

        let data_partial_null = json!({
            "user": {
                "profile": null
            }
        });

        let (result, errors) = JSONSelection::parse("$.user?.profile?.name")
            .unwrap()
            .apply_to(&data_partial_null);
        assert!(errors.is_empty());
        assert_eq!(result, Some(json!(null)));
    }

    #[test]
    fn test_optional_method_on_null() {
        use serde_json_bytes::json;

        let data = json!({
            "items": null
        });

        let (result, errors) = JSONSelection::parse("$.items?->first")
            .unwrap()
            .apply_to(&data);
        assert!(errors.is_empty());
        assert_eq!(result, Some(json!(null)));
    }

    #[test]
    fn test_optional_method_with_valid_method() {
        use serde_json_bytes::json;

        let data = json!({
            "values": [1, 2, 3]
        });

        let (result, errors) = JSONSelection::parse("$.values?->first")
            .unwrap()
            .apply_to(&data);
        assert!(errors.is_empty());
        assert_eq!(result, Some(json!(1)));
    }

    #[test]
    fn test_optional_method_with_unknown_method() {
        use serde_json_bytes::json;

        let data = json!({
            "values": [1, 2, 3]
        });

        let (result, errors) = JSONSelection::parse("$.values?->length")
            .unwrap()
            .apply_to(&data);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message().contains("Method ?->length not found"));
        assert_eq!(result, None);
    }

    #[test]
    fn test_optional_chaining_with_subselection_on_valid_data() {
        use serde_json_bytes::json;

        let data = json!({
            "user": {
                "profile": {
                    "name": "Alice",
                    "age": 30,
                    "email": "alice@example.com"
                }
            }
        });

        let (result, errors) = JSONSelection::parse("$.user?.profile { name age }")
            .unwrap()
            .apply_to(&data);
        assert!(errors.is_empty());
        assert_eq!(
            result,
            Some(json!({
                "name": "Alice",
                "age": 30
            }))
        );
    }

    #[test]
    fn test_optional_chaining_with_subselection_on_null_data() {
        use serde_json_bytes::json;

        let data_null = json!({
            "user": null
        });

        let (result, errors) = JSONSelection::parse("$.user?.profile { name age }")
            .unwrap()
            .apply_to(&data_null);
        assert!(errors.is_empty());
        assert_eq!(result, Some(json!(null)));
    }

    #[test]
    fn test_mixed_regular_and_optional_chaining_working_case() {
        use serde_json_bytes::json;

        let data = json!({
            "response": {
                "data": {
                    "user": {
                        "profile": {
                            "name": "Bob"
                        }
                    }
                }
            }
        });

        let (result, errors) = JSONSelection::parse("$.response.data?.user.profile.name")
            .unwrap()
            .apply_to(&data);
        assert!(errors.is_empty());
        assert_eq!(result, Some(json!("Bob")));
    }

    #[test]
    fn test_mixed_regular_and_optional_chaining_with_null() {
        use serde_json_bytes::json;

        let data_null_data = json!({
            "response": {
                "data": null
            }
        });

        let (result, errors) = JSONSelection::parse("$.response.data?.user.profile.name")
            .unwrap()
            .apply_to(&data_null_data);
        assert!(errors.is_empty());
        assert_eq!(result, Some(json!(null)));
    }

    #[test]
    fn test_optional_selection_set_with_valid_data() {
        use serde_json_bytes::json;

        let data = json!({
            "user": {
                "id": 123,
                "name": "Alice"
            }
        });

        let (result, errors) = JSONSelection::parse("$.user ?{ id name }")
            .unwrap()
            .apply_to(&data);
        assert_eq!(
            result,
            Some(json!({
                "id": 123,
                "name": "Alice"
            }))
        );
        assert_eq!(errors, vec![]);
    }

    #[test]
    fn test_optional_selection_set_with_null_data() {
        use serde_json_bytes::json;

        let data = json!({
            "user": null
        });

        let (result, errors) = JSONSelection::parse("$.user ?{ id name }")
            .unwrap()
            .apply_to(&data);
        assert_eq!(result, Some(json!(null)));
        assert_eq!(errors, vec![]);
    }

    #[test]
    fn test_optional_selection_set_with_missing_property() {
        use serde_json_bytes::json;

        let data = json!({
            "other": "value"
        });

        let (result, errors) = JSONSelection::parse("$.user ?{ id name }")
            .unwrap()
            .apply_to(&data);
        assert_eq!(result, None);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message().contains("Property .user not found"));
    }

    #[test]
    fn test_optional_selection_set_with_non_object() {
        use serde_json_bytes::json;

        let data = json!({
            "user": "not an object"
        });

        let (result, errors) = JSONSelection::parse("$.user ?{ id name }")
            .unwrap()
            .apply_to(&data);
        // When data is not null but not an object, SubSelection still tries to access properties
        // This results in errors, but returns the original value since no properties were found
        assert_eq!(result, Some(json!("not an object")));
        assert_eq!(errors.len(), 2);
        assert!(
            errors[0]
                .message()
                .contains("Property .id not found in string")
        );
        assert!(
            errors[1]
                .message()
                .contains("Property .name not found in string")
        );
    }

    #[test]
    fn test_nested_optional_selection_sets() {
        use serde_json_bytes::json;

        let data = json!({
            "user": {
                "profile": {
                    "name": "Alice",
                    "email": "alice@example.com"
                }
            }
        });

        let (result, errors) = JSONSelection::parse("$.user.profile ?{ name email }")
            .unwrap()
            .apply_to(&data);
        assert_eq!(
            result,
            Some(json!({
                "name": "Alice",
                "email": "alice@example.com"
            }))
        );
        assert_eq!(errors, vec![]);

        // Test with null nested data
        let data_with_null_profile = json!({
            "user": {
                "profile": null
            }
        });

        let (result, errors) = JSONSelection::parse("$.user.profile ?{ name email }")
            .unwrap()
            .apply_to(&data_with_null_profile);
        assert_eq!(result, Some(json!(null)));
        assert_eq!(errors, vec![]);
    }

    #[test]
    fn test_mixed_optional_selection_and_optional_chaining() {
        use serde_json_bytes::json;

        let data = json!({
            "user": {
                "id": 123,
                "profile": null
            }
        });

        let (result, errors) = JSONSelection::parse("$.user ?{ id profileName: profile?.name }")
            .unwrap()
            .apply_to(&data);
        assert_eq!(
            result,
            Some(json!({
                "id": 123,
                "profileName": null
            }))
        );
        assert_eq!(errors, vec![]);

        // Test with missing user
        let data_no_user = json!({
            "other": "value"
        });

        let (result, errors) = JSONSelection::parse("$.user ?{ id profileName: profile?.name }")
            .unwrap()
            .apply_to(&data_no_user);
        assert_eq!(result, None);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message().contains("Property .user not found"));
    }

    #[test]
    fn test_optional_selection_set_parsing() {
        // Test that the parser correctly handles optional selection sets
        let selection = JSONSelection::parse("$.user ?{ id name }").unwrap();
        assert_eq!(selection.pretty_print(), "$.user ?{\n  id\n  name\n}");

        // Test with nested optional selection sets
        let selection = JSONSelection::parse("$.user.profile ?{ name }").unwrap();
        assert_eq!(selection.pretty_print(), "$.user.profile ?{\n  name\n}");

        // Test mixed with regular selection sets
        let selection = JSONSelection::parse("$.user ?{ id profile { name } }").unwrap();
        assert_eq!(
            selection.pretty_print(),
            "$.user ?{\n  id\n  profile {\n    name\n  }\n}"
        );
    }

    #[test]
    fn test_optional_selection_set_with_arrays() {
        use serde_json_bytes::json;

        let data = json!({
            "users": [
                {
                    "id": 1,
                    "name": "Alice"
                },
                null,
                {
                    "id": 3,
                    "name": "Charlie"
                }
            ]
        });

        let (result, errors) = JSONSelection::parse("$.users ?{ id name }")
            .unwrap()
            .apply_to(&data);
        assert_eq!(
            result,
            Some(json!([
                {
                    "id": 1,
                    "name": "Alice"
                },
                null,
                {
                    "id": 3,
                    "name": "Charlie"
                }
            ]))
        );
        // When applying selection to arrays, null elements cause errors when trying to access properties
        assert_eq!(errors.len(), 2);
        assert!(
            errors[0]
                .message()
                .contains("Property .id not found in null")
        );
        assert!(
            errors[1]
                .message()
                .contains("Property .name not found in null")
        );
    }
}
