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
use shape::location::Location;
use shape::location::SourceId;

use super::helpers::json_merge;
use super::helpers::json_type_name;
use super::immutable::InputPath;
use super::known_var::KnownVariable;
use super::lit_expr::LitExpr;
use super::lit_expr::LitOp;
use super::location::OffsetRange;
use super::location::Ranged;
use super::location::WithRange;
use super::methods::ArrowMethod;
use super::parser::*;
use crate::connectors::spec::ConnectSpec;

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

        let spec = self.spec();
        let (value, apply_errors) =
            self.apply_to_path(data, &vars_with_paths, &InputPath::empty(), spec);

        // Since errors is an IndexSet, this line effectively deduplicates the
        // errors, in an attempt to make them less verbose. However, now that we
        // include both path and range information in the errors, there's an
        // argument to be made that errors can no longer be meaningfully
        // deduplicated, so we might consider sticking with a Vec<ApplyToError>.
        errors.extend(apply_errors);

        (value, errors.into_iter().collect())
    }

    pub fn shape(&self) -> Shape {
        let context =
            ShapeContext::new(SourceId::Other("JSONSelection".into())).with_spec(self.spec());

        self.compute_output_shape(
            // Relatively static/unchanging inputs to compute_output_shape,
            // passed down by immutable shared reference.
            &context,
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
        )
    }

    pub(crate) fn compute_output_shape(&self, context: &ShapeContext, input_shape: Shape) -> Shape {
        debug_assert_eq!(context.spec(), self.spec());

        let computable: &dyn ApplyToInternal = match &self.inner {
            TopLevelSelection::Named(selection) => selection,
            TopLevelSelection::Path(path_selection) => path_selection,
        };

        let dollar_shape = input_shape.clone();

        if Some(&input_shape) == context.named_shapes().get("$root") {
            // If the $root variable happens to be bound to the input shape,
            // context does not need to be cloned or modified.
            computable.compute_output_shape(context, input_shape, dollar_shape)
        } else {
            // Otherwise, we'll want to register the input_shape as $root in a
            // cloned_context, so $root is reliably defined either way.
            let cloned_context = context
                .clone()
                .with_named_shapes([("$root".to_string(), input_shape.clone())]);
            computable.compute_output_shape(&cloned_context, input_shape, dollar_shape)
        }
    }
}

impl Ranged for JSONSelection {
    fn range(&self) -> OffsetRange {
        match &self.inner {
            TopLevelSelection::Named(selection) => selection.range(),
            TopLevelSelection::Path(path_selection) => path_selection.range(),
        }
    }

    fn shape_location(&self, source_id: &SourceId) -> Option<Location> {
        self.range().map(|range| source_id.location(range))
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
        spec: ConnectSpec,
    ) -> (Option<JSON>, Vec<ApplyToError>);

    // When array is encountered, the Self selection will be applied to each
    // element of the array, producing a new array.
    fn apply_to_array(
        &self,
        data_array: &[JSON],
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
        spec: ConnectSpec,
    ) -> (Option<JSON>, Vec<ApplyToError>) {
        let mut output = Vec::with_capacity(data_array.len());
        let mut errors = Vec::new();

        for (i, element) in data_array.iter().enumerate() {
            let input_path_with_index = input_path.append(json!(i));
            let (applied, apply_errors) =
                self.apply_to_path(element, vars, &input_path_with_index, spec);
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
        context: &ShapeContext,
        // Shape of the `@` variable, which typically changes with each
        // recursive call to compute_output_shape.
        input_shape: Shape,
        // Shape of the `$` variable, which is bound to the closest enclosing
        // subselection object, or the root data object if there is no enclosing
        // subselection.
        dollar_shape: Shape,
    ) -> Shape;
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct ShapeContext {
    /// [`ConnectSpec`] version derived from the [`JSONSelection`] that created
    /// this [`ShapeContext`].
    #[allow(dead_code)]
    spec: ConnectSpec,

    /// Shapes of other named variables, with the variable name `String`
    /// including the initial `$` character. This map typically does not change
    /// during the compute_output_shape recursion, and so can be passed down by
    /// immutable reference.
    named_shapes: IndexMap<String, Shape>,

    /// A shared source name to use for all locations originating from this
    /// `JSONSelection`.
    source_id: SourceId,
}

impl ShapeContext {
    pub(crate) fn new(source_id: SourceId) -> Self {
        Self {
            spec: JSONSelection::default_connect_spec(),
            named_shapes: IndexMap::default(),
            source_id,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn spec(&self) -> ConnectSpec {
        self.spec
    }

    pub(crate) fn with_spec(mut self, spec: ConnectSpec) -> Self {
        self.spec = spec;
        self
    }

    pub(crate) fn named_shapes(&self) -> &IndexMap<String, Shape> {
        &self.named_shapes
    }

    pub(crate) fn with_named_shapes(
        mut self,
        named_shapes: impl IntoIterator<Item = (String, Shape)>,
    ) -> Self {
        for (name, shape) in named_shapes {
            self.named_shapes.insert(name.clone(), shape.clone());
        }
        self
    }

    pub(crate) fn source_id(&self) -> &SourceId {
        &self.source_id
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub struct ApplyToError {
    message: String,
    path: Vec<JSON>,
    range: OffsetRange,
    spec: ConnectSpec,
}

impl ApplyToError {
    pub(crate) const fn new(
        message: String,
        path: Vec<JSON>,
        range: OffsetRange,
        spec: ConnectSpec,
    ) -> Self {
        Self {
            message,
            path,
            range,
            spec,
        }
    }

    // This macro is useful for tests, but it absolutely should never be used with
    // dynamic input at runtime, since it panics for any input that's not JSON.
    #[cfg(test)]
    pub(crate) fn from_json(json: &JSON) -> Self {
        use crate::link::spec::Version;

        let error = json.as_object().unwrap();
        let message = error.get("message").unwrap().as_str().unwrap().to_string();
        let path = error.get("path").unwrap().as_array().unwrap().clone();
        let range = error.get("range").unwrap().as_array().unwrap();
        let spec = error
            .get("spec")
            .and_then(|s| s.as_str())
            .and_then(|s| match s.parse::<Version>() {
                Ok(version) => ConnectSpec::try_from(&version).ok(),
                Err(_) => None,
            })
            .unwrap_or_else(ConnectSpec::latest);

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
            spec,
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

    pub fn spec(&self) -> ConnectSpec {
        self.spec
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
        _spec: ConnectSpec,
    ) -> (Option<JSON>, Vec<ApplyToError>) {
        match &self.inner {
            // Because we represent a JSONSelection::Named as a SubSelection, we
            // can fully delegate apply_to_path to SubSelection::apply_to_path.
            // Even if we represented Self::Named as a Vec<NamedSelection>, we
            // could still delegate to SubSelection::apply_to_path, but we would
            // need to create a temporary SubSelection to wrap the selections
            // Vec.
            TopLevelSelection::Named(named_selections) => {
                named_selections.apply_to_path(data, vars, input_path, self.spec)
            }
            TopLevelSelection::Path(path_selection) => {
                path_selection.apply_to_path(data, vars, input_path, self.spec)
            }
        }
    }

    fn compute_output_shape(
        &self,
        context: &ShapeContext,
        input_shape: Shape,
        dollar_shape: Shape,
    ) -> Shape {
        debug_assert_eq!(context.spec(), self.spec());

        match &self.inner {
            TopLevelSelection::Named(selection) => {
                selection.compute_output_shape(context, input_shape, dollar_shape)
            }
            TopLevelSelection::Path(path_selection) => {
                path_selection.compute_output_shape(context, input_shape, dollar_shape)
            }
        }
    }
}

impl ApplyToInternal for NamedSelection {
    fn apply_to_path(
        &self,
        data: &JSON,
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
        spec: ConnectSpec,
    ) -> (Option<JSON>, Vec<ApplyToError>) {
        let mut output: Option<JSON> = None;
        let mut errors = Vec::new();

        let (value_opt, apply_errors) = self.path.apply_to_path(data, vars, input_path, spec);
        errors.extend(apply_errors);

        match &self.prefix {
            NamingPrefix::Alias(alias) => {
                if let Some(value) = value_opt {
                    output = Some(json!({ alias.name.as_str(): value }));
                }
            }

            NamingPrefix::Spread(_spread_range) => {
                match value_opt {
                    Some(JSON::Object(_) | JSON::Null) => {
                        // Objects and null are valid outputs for an
                        // inline/spread NamedSelection.
                        output = value_opt;
                    }
                    Some(value) => {
                        errors.push(ApplyToError::new(
                            format!("Expected object or null, not {}", json_type_name(&value)),
                            input_path.to_vec(),
                            self.path.range(),
                            spec,
                        ));
                    }
                    None => {
                        errors.push(ApplyToError::new(
                            "Inlined path produced no value".to_string(),
                            input_path.to_vec(),
                            self.path.range(),
                            spec,
                        ));
                    }
                };
            }

            NamingPrefix::None => {
                // Since there is no prefix (NamingPrefix::None), value_opt is
                // usable as the output of NamedSelection::apply_to_path only if
                // the NamedSelection has an implied single key, or by having a
                // trailing SubSelection that guarantees object/null output.
                if let Some(single_key) = self.path.get_single_key() {
                    if let Some(value) = value_opt {
                        output = Some(json!({ single_key.as_str(): value }));
                    }
                } else {
                    output = value_opt;
                }
            }
        }

        (output, errors)
    }

    fn compute_output_shape(
        &self,
        context: &ShapeContext,
        input_shape: Shape,
        dollar_shape: Shape,
    ) -> Shape {
        let path_shape = self
            .path
            .compute_output_shape(context, input_shape, dollar_shape);

        if let Some(single_output_key) = self.get_single_key() {
            let mut map = Shape::empty_map();
            map.insert(single_output_key.as_string(), path_shape);
            Shape::record(map, self.shape_location(context.source_id()))
        } else {
            path_shape
        }
    }
}

impl ApplyToInternal for PathSelection {
    fn apply_to_path(
        &self,
        data: &JSON,
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
        spec: ConnectSpec,
    ) -> (Option<JSON>, Vec<ApplyToError>) {
        match (self.path.as_ref(), vars.get(&KnownVariable::Dollar)) {
            // If this is a KeyPath, instead of using data as given, we need to
            // evaluate the path starting from the current value of $. To evaluate
            // the KeyPath against data, prefix it with @. This logic supports
            // method chaining like obj->has('a')->and(obj->has('b')), where both
            // obj references are interpreted as $.obj.
            (PathList::Key(_, _), Some((dollar_data, dollar_path))) => {
                self.path
                    .apply_to_path(dollar_data, vars, dollar_path, spec)
            }

            // If $ is undefined for some reason, fall back to using data...
            // TODO: Since $ should never be undefined, we might want to
            // guarantee its existence at compile time, somehow.
            // (PathList::Key(_, _), None) => todo!(),
            _ => self.path.apply_to_path(data, vars, input_path, spec),
        }
    }

    fn compute_output_shape(
        &self,
        context: &ShapeContext,
        input_shape: Shape,
        dollar_shape: Shape,
    ) -> Shape {
        match self.path.as_ref() {
            PathList::Key(_, _) => {
                // If this is a KeyPath, we need to evaluate the path starting
                // from the current $ shape, so we pass dollar_shape as the data
                // *and* dollar_shape to self.path.compute_output_shape.
                self.path
                    .compute_output_shape(context, dollar_shape.clone(), dollar_shape)
            }
            // If this is not a KeyPath, keep evaluating against input_shape.
            // This logic parallels PathSelection::apply_to_path (above).
            _ => self
                .path
                .compute_output_shape(context, input_shape, dollar_shape),
        }
    }
}

impl ApplyToInternal for WithRange<PathList> {
    fn apply_to_path(
        &self,
        data: &JSON,
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
        spec: ConnectSpec,
    ) -> (Option<JSON>, Vec<ApplyToError>) {
        match self.as_ref() {
            PathList::Var(ranged_var_name, tail) => {
                let var_name = ranged_var_name.as_ref();
                if var_name == &KnownVariable::AtSign {
                    // We represent @ as a variable name in PathList::Var, but
                    // it is never stored in the vars map, because it is always
                    // shorthand for the current data value.
                    tail.apply_to_path(data, vars, input_path, spec)
                } else if let Some((var_data, var_path)) = vars.get(var_name) {
                    // Variables are associated with a path, which is always
                    // just the variable name for named $variables other than $.
                    // For the special variable $, the path represents the
                    // sequence of keys from the root input data to the $ data.
                    tail.apply_to_path(var_data, vars, var_path, spec)
                } else {
                    (
                        None,
                        vec![ApplyToError::new(
                            format!("Variable {} not found", var_name.as_str()),
                            input_path.to_vec(),
                            ranged_var_name.range(),
                            spec,
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
                        .apply_to_array(array, vars, input_path, spec)
                        .and_then_collecting_errors(|shallow_mapped_array| {
                            // This tail.apply_to_path call happens only once,
                            // passing to the original/top-level tail the entire
                            // array produced by key-related recursion/mapping.
                            tail.apply_to_path(
                                shallow_mapped_array,
                                vars,
                                &input_path_with_key,
                                spec,
                            )
                        })
                } else {
                    let not_found = || {
                        (
                            None,
                            vec![ApplyToError::new(
                                format!(
                                    "Property {} not found in {}",
                                    key.dotted(),
                                    json_type_name(data),
                                ),
                                input_path_with_key.to_vec(),
                                key.range(),
                                spec,
                            )],
                        )
                    };

                    if !matches!(data, JSON::Object(_)) {
                        return not_found();
                    }

                    if let Some(child) = data.get(key.as_str()) {
                        tail.apply_to_path(child, vars, &input_path_with_key, spec)
                    } else if tail.is_question() {
                        (None, vec![])
                    } else {
                        not_found()
                    }
                }
            }
            PathList::Expr(expr, tail) => expr
                .apply_to_path(data, vars, input_path, spec)
                .and_then_collecting_errors(|value| {
                    tail.apply_to_path(value, vars, input_path, spec)
                }),
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
                                spec,
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
                            spec,
                        );

                        if let Some(result) = result_opt {
                            tail.apply_to_path(&result, vars, &method_path, spec)
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
            PathList::Selection(selection) => selection.apply_to_path(data, vars, input_path, spec),
            PathList::Question(tail) => {
                // Universal null check for any operation after ?
                if data.is_null() {
                    (None, vec![])
                } else {
                    tail.apply_to_path(data, vars, input_path, spec)
                }
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
        context: &ShapeContext,
        input_shape: Shape,
        dollar_shape: Shape,
    ) -> Shape {
        if input_shape.is_none() {
            // If the previous path prefix evaluated to None, path evaluation
            // must terminate because there is no JSON value to pass as the
            // input_shape to the rest of the path, so the output shape of the
            // whole path must be None. Any errors that might explain an
            // unexpected None value should already have been reported as
            // Shape::error_with_partial errors at a higher level.
            return input_shape;
        }

        match input_shape.case() {
            ShapeCase::One(shapes) => {
                return Shape::one(
                    shapes.iter().map(|shape| {
                        self.compute_output_shape(context, shape.clone(), dollar_shape.clone())
                    }),
                    input_shape.locations.iter().cloned(),
                );
            }
            ShapeCase::All(shapes) => {
                return Shape::all(
                    shapes.iter().map(|shape| {
                        self.compute_output_shape(context, shape.clone(), dollar_shape.clone())
                    }),
                    input_shape.locations.iter().cloned(),
                );
            }
            ShapeCase::Error(error) => {
                return match error.partial.as_ref() {
                    Some(partial) => Shape::error_with_partial(
                        error.message.clone(),
                        self.compute_output_shape(context, partial.clone(), dollar_shape),
                        input_shape.locations.iter().cloned(),
                    ),
                    None => input_shape.clone(),
                };
            }
            _ => {}
        };

        // Given the base cases above, we can assume below that input_shape is
        // neither ::One, ::All, nor ::Error.

        let (current_shape, tail_opt) = match self.as_ref() {
            PathList::Var(ranged_var_name, tail) => {
                let var_name = ranged_var_name.as_ref();
                let var_shape = if var_name == &KnownVariable::AtSign {
                    input_shape
                } else if var_name == &KnownVariable::Dollar {
                    dollar_shape.clone()
                } else if let Some(shape) = context.named_shapes().get(var_name.as_str()) {
                    shape.clone()
                } else {
                    Shape::name(
                        var_name.as_str(),
                        ranged_var_name.shape_location(context.source_id()),
                    )
                };
                (var_shape, Some(tail))
            }

            // For the first key in a path, PathSelection::compute_output_shape
            // will have set our input_shape equal to its dollar_shape, thereby
            // ensuring that some.nested.path is equivalent to
            // $.some.nested.path.
            PathList::Key(key, tail) => {
                let child_shape = field(&input_shape, key, context.source_id());

                // Here input_shape was not None, but input_shape.field(key) was
                // None, so it's the responsibility of this PathList::Key node
                // to report the missing property error. Elsewhere None may
                // terminate path evaluation, but it does not necessarily
                // trigger a Shape::error. Here, the shape system is telling us
                // the key will never be found, so an error is warranted.
                //
                // In the future, we might allow tail to be a PathList::Question
                // supporting optional ? chaining syntax, which would be a way
                // of silencing this error when the key's absence is acceptable.
                if child_shape.is_none() {
                    return Shape::error(
                        format!(
                            "Property {} not found in {}",
                            key.dotted(),
                            input_shape.pretty_print()
                        ),
                        key.shape_location(context.source_id()),
                    );
                }

                (child_shape, Some(tail))
            }

            PathList::Expr(expr, tail) => (
                expr.compute_output_shape(context, input_shape, dollar_shape.clone()),
                Some(tail),
            ),

            PathList::Method(method_name, method_args, tail) => {
                if let Some(method) = ArrowMethod::lookup(method_name) {
                    // Before connect/v0.3, we did not consult method.shape at
                    // all, and instead returned Unknown. Since this behavior
                    // has consequences for URI validation, the older behavior
                    // is preserved/retrievable given ConnectSpec::V0_2/earlier.
                    if context.spec() < ConnectSpec::V0_3 {
                        (
                            Shape::unknown(method_name.shape_location(context.source_id())),
                            None,
                        )
                    } else {
                        (
                            method.shape(
                                context,
                                method_name,
                                method_args.as_ref(),
                                input_shape,
                                dollar_shape.clone(),
                            ),
                            Some(tail),
                        )
                    }
                } else {
                    (
                        Shape::error(
                            format!("Method ->{} not found", method_name.as_str()),
                            method_name.shape_location(context.source_id()),
                        ),
                        None,
                    )
                }
            }

            PathList::Question(tail) => {
                // Optional operation always produces nullable output
                let result_shape =
                    tail.compute_output_shape(context, input_shape, dollar_shape.clone());
                // Make result nullable since optional chaining can produce null
                (
                    Shape::one(
                        [
                            result_shape,
                            Shape::none().with_locations(self.shape_location(context.source_id())),
                        ],
                        self.shape_location(context.source_id()),
                    ),
                    None,
                )
            }

            PathList::Selection(selection) => (
                selection.compute_output_shape(context, input_shape, dollar_shape.clone()),
                None,
            ),

            PathList::Empty => (input_shape, None),
        };

        if let Some(tail) = tail_opt {
            tail.compute_output_shape(context, current_shape, dollar_shape)
        } else {
            current_shape
        }
    }
}

impl ApplyToInternal for WithRange<LitExpr> {
    fn apply_to_path(
        &self,
        data: &JSON,
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
        spec: ConnectSpec,
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
                    let (value_opt, apply_errors) =
                        value.apply_to_path(data, vars, input_path, spec);
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
                    let (value_opt, apply_errors) =
                        value.apply_to_path(data, vars, input_path, spec);
                    errors.extend(apply_errors);
                    output.push(value_opt.unwrap_or(JSON::Null));
                }
                (Some(JSON::Array(output)), errors)
            }
            LitExpr::Path(path) => path.apply_to_path(data, vars, input_path, spec),
            LitExpr::LitPath(literal, subpath) => literal
                .apply_to_path(data, vars, input_path, spec)
                .and_then_collecting_errors(|value| {
                    subpath.apply_to_path(value, vars, input_path, spec)
                }),
            LitExpr::OpChain(op, operands) => {
                match op.as_ref() {
                    LitOp::NullishCoalescing => {
                        // Null coalescing: A ?? B ?? C
                        // Returns B if A is null OR None, otherwise A. If B is also null/None, returns C, etc.
                        let mut accumulated_errors = Vec::new();
                        let mut last_value: Option<JSON> = None;

                        for operand in operands {
                            let (value, errors) =
                                operand.apply_to_path(data, vars, input_path, spec);

                            match value {
                                // If we get a non-null, non-None value, return it
                                Some(JSON::Null) | None => {
                                    // Accumulate errors but continue to next operand
                                    accumulated_errors.extend(errors);
                                    last_value = value;
                                    continue;
                                }
                                Some(value) => {
                                    // Found a non-null/non-None value, return it (ignoring accumulated errors)
                                    return (Some(value), errors);
                                }
                            }
                        }

                        // If the last value was Some(JSON::Null), we return
                        // that null, since there is no ?? after it. Otherwise,
                        // last_value will be None at this point, because we
                        // return Some(value) above as soon as we find a
                        // non-null/non-None value.
                        if last_value.is_none() {
                            // If we never found a non-null value, return None
                            // with all accumulated errors.
                            (None, accumulated_errors)
                        } else {
                            // If the last operand evaluated to null (or
                            // anything else except None), that counts as a
                            // successful evaluation, so we do not return any
                            // earlier accumulated_errors.
                            (last_value, Vec::new())
                        }
                    }

                    LitOp::NoneCoalescing => {
                        // None coalescing: A ?! B ?! C
                        // Returns B if A is None (preserves null), otherwise A. If B is also None, returns C, etc.
                        let mut accumulated_errors = Vec::new();

                        for operand in operands {
                            let (value, errors) =
                                operand.apply_to_path(data, vars, input_path, spec);

                            match value {
                                // If we get None, continue to next operand
                                None => {
                                    accumulated_errors.extend(errors);
                                    continue;
                                }
                                // If we get any value (including null), return it
                                Some(value) => {
                                    return (Some(value), errors);
                                }
                            }
                        }

                        // All operands were None, return None with all accumulated errors
                        (None, accumulated_errors)
                    }
                }
            }
        }
    }

    fn compute_output_shape(
        &self,
        context: &ShapeContext,
        input_shape: Shape,
        dollar_shape: Shape,
    ) -> Shape {
        let locations = self.shape_location(context.source_id());

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
                            context,
                            input_shape.clone(),
                            dollar_shape.clone(),
                        ),
                    );
                }
                Shape::object(fields, Shape::none(), locations)
            }

            LitExpr::Array(vec) => {
                let mut shapes = Vec::with_capacity(vec.len());
                for value in vec {
                    shapes.push(value.compute_output_shape(
                        context,
                        input_shape.clone(),
                        dollar_shape.clone(),
                    ));
                }
                Shape::array(shapes, Shape::none(), locations)
            }

            LitExpr::Path(path) => path.compute_output_shape(context, input_shape, dollar_shape),

            LitExpr::LitPath(literal, subpath) => {
                let literal_shape =
                    literal.compute_output_shape(context, input_shape, dollar_shape.clone());
                subpath.compute_output_shape(context, literal_shape, dollar_shape)
            }

            LitExpr::OpChain(op, operands) => {
                match op.as_ref() {
                    LitOp::NullishCoalescing | LitOp::NoneCoalescing => {
                        let shapes: Vec<Shape> = operands
                            .iter()
                            .map(|operand| {
                                operand.compute_output_shape(
                                    context,
                                    input_shape.clone(),
                                    dollar_shape.clone(),
                                )
                            })
                            .collect();

                        // Create a union of all possible shapes
                        Shape::one(shapes, locations)
                    }
                }
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
        spec: ConnectSpec,
    ) -> (Option<JSON>, Vec<ApplyToError>) {
        if let JSON::Array(array) = data {
            return self.apply_to_array(array, vars, input_path, spec);
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
                named_selection.apply_to_path(data, &vars, input_path, spec);
            errors.extend(apply_errors);

            let (merged, merge_errors) = json_merge(Some(&output), named_output_opt.as_ref());

            errors.extend(merge_errors.into_iter().map(|message| {
                ApplyToError::new(message, input_path.to_vec(), self.range(), spec)
            }));

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
        context: &ShapeContext,
        input_shape: Shape,
        _previous_dollar_shape: Shape,
    ) -> Shape {
        // Just as SubSelection::apply_to_path calls apply_to_array when data is
        // an array, so compute_output_shape recursively computes the output
        // shapes of each array element shape.
        if let ShapeCase::Array { prefix, tail } = input_shape.case() {
            let new_prefix = prefix
                .iter()
                .map(|shape| self.compute_output_shape(context, shape.clone(), shape.clone()))
                .collect::<Vec<_>>();

            let new_tail = if tail.is_none() {
                tail.clone()
            } else {
                self.compute_output_shape(context, tail.clone(), tail.clone())
            };

            return Shape::array(
                new_prefix,
                new_tail,
                self.shape_location(context.source_id()),
            );
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
        let mut all_shape = Shape::none();

        for named_selection in self.selections.iter() {
            // Simplifying as we go with Shape::all keeps all_shape relatively
            // small in the common case when all named_selection items return an
            // object shape, since those object shapes can all be merged
            // together into one object.
            all_shape = Shape::all(
                [
                    all_shape,
                    named_selection.compute_output_shape(
                        context,
                        input_shape.clone(),
                        dollar_shape.clone(),
                    ),
                ],
                self.shape_location(context.source_id()),
            );

            // If any named_selection item returns null instead of an object,
            // that nullifies the whole object and allows shape computation to
            // bail out early.
            if all_shape.is_null() {
                break;
            }
        }

        if all_shape.is_none() {
            Shape::empty_object(self.shape_location(context.source_id()))
        } else {
            all_shape
        }
    }
}

/// Helper to get the field from a shape or error if the object doesn't have that field.
fn field(shape: &Shape, key: &WithRange<Key>, source_id: &SourceId) -> Shape {
    if let ShapeCase::One(inner) = shape.case() {
        let mut new_fields = Vec::new();
        for inner_field in inner {
            new_fields.push(field(inner_field, key, source_id));
        }
        return Shape::one(new_fields, shape.locations.iter().cloned());
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
    use rstest::rstest;

    use super::*;
    use crate::assert_debug_snapshot;
    use crate::connectors::json_selection::PrettyPrintable;
    use crate::selection;

    #[rstest]
    #[case::v0_2(ConnectSpec::V0_2)]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_apply_to_selection(#[case] spec: ConnectSpec) {
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

        #[track_caller]
        fn check_ok(data: &JSON, selection: JSONSelection, expected_json: JSON) {
            let (actual_json, errors) = selection.apply_to(data);
            assert_eq!(actual_json, Some(expected_json));
            assert_eq!(errors, vec![]);
        }

        check_ok(&data, selection!("hello", spec), json!({"hello": "world"}));

        check_ok(
            &data,
            selection!("nested", spec),
            json!({
                "nested": {
                    "hello": "world",
                    "world": "hello",
                },
            }),
        );

        check_ok(&data, selection!("nested.hello", spec), json!("world"));
        check_ok(&data, selection!("$.nested.hello", spec), json!("world"));

        check_ok(&data, selection!("nested.world", spec), json!("hello"));
        check_ok(&data, selection!("$.nested.world", spec), json!("hello"));

        check_ok(
            &data,
            selection!("nested hello", spec),
            json!({
                "hello": "world",
                "nested": {
                    "hello": "world",
                    "world": "hello",
                },
            }),
        );

        check_ok(
            &data,
            selection!("array { hello }", spec),
            json!({
                "array": [
                    { "hello": "world 0" },
                    { "hello": "world 1" },
                    { "hello": "world 2" },
                ],
            }),
        );

        check_ok(
            &data,
            selection!("greetings: array { hello }", spec),
            json!({
                "greetings": [
                    { "hello": "world 0" },
                    { "hello": "world 1" },
                    { "hello": "world 2" },
                ],
            }),
        );

        check_ok(
            &data,
            selection!("$.array { hello }", spec),
            json!([
                { "hello": "world 0" },
                { "hello": "world 1" },
                { "hello": "world 2" },
            ]),
        );

        check_ok(
            &data,
            selection!("worlds: array.hello", spec),
            json!({
                "worlds": [
                    "world 0",
                    "world 1",
                    "world 2",
                ],
            }),
        );

        check_ok(
            &data,
            selection!("worlds: $.array.hello", spec),
            json!({
                "worlds": [
                    "world 0",
                    "world 1",
                    "world 2",
                ],
            }),
        );

        check_ok(
            &data,
            selection!("array.hello", spec),
            json!(["world 0", "world 1", "world 2",]),
        );

        check_ok(
            &data,
            selection!("$.array.hello", spec),
            json!(["world 0", "world 1", "world 2",]),
        );

        check_ok(
            &data,
            selection!("nested grouped: { hello worlds: array.hello }", spec),
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
            &data,
            selection!("nested grouped: { hello worlds: $.array.hello }", spec),
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

    #[rstest]
    #[case::v0_2(ConnectSpec::V0_2)]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_apply_to_errors(#[case] spec: ConnectSpec) {
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
            selection!("hello", spec).apply_to(&data),
            (Some(json!({"hello": "world"})), vec![],)
        );

        fn make_yellow_errors_expected(
            yellow_range: std::ops::Range<usize>,
            spec: ConnectSpec,
        ) -> Vec<ApplyToError> {
            vec![ApplyToError::new(
                "Property .yellow not found in object".to_string(),
                vec![json!("yellow")],
                Some(yellow_range),
                spec,
            )]
        }
        assert_eq!(
            selection!("yellow", spec).apply_to(&data),
            (Some(json!({})), make_yellow_errors_expected(0..6, spec)),
        );
        assert_eq!(
            selection!("$.yellow", spec).apply_to(&data),
            (None, make_yellow_errors_expected(2..8, spec)),
        );

        assert_eq!(
            selection!("nested.hello", spec).apply_to(&data),
            (Some(json!(123)), vec![],)
        );

        fn make_quoted_yellow_expected(
            yellow_range: std::ops::Range<usize>,
            spec: ConnectSpec,
        ) -> (Option<JSON>, Vec<ApplyToError>) {
            (
                None,
                vec![ApplyToError::new(
                    "Property .\"yellow\" not found in object".to_string(),
                    vec![json!("nested"), json!("yellow")],
                    Some(yellow_range),
                    spec,
                )],
            )
        }
        assert_eq!(
            selection!("nested.'yellow'", spec).apply_to(&data),
            make_quoted_yellow_expected(7..15, spec),
        );
        assert_eq!(
            selection!("nested.\"yellow\"", spec).apply_to(&data),
            make_quoted_yellow_expected(7..15, spec),
        );
        assert_eq!(
            selection!("$.nested.'yellow'", spec).apply_to(&data),
            make_quoted_yellow_expected(9..17, spec),
        );

        fn make_nested_path_expected(
            hola_range: (usize, usize),
            yellow_range: (usize, usize),
            spec: ConnectSpec,
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
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .yellow not found in object",
                        "path": ["nested", "yellow"],
                        "range": yellow_range,
                        "spec": spec.to_string(),
                    })),
                ],
            )
        }
        assert_eq!(
            selection!("$.nested { hola yellow world }", spec).apply_to(&data),
            make_nested_path_expected((11, 15), (16, 22), spec),
        );
        assert_eq!(
            selection!(" $ . nested { hola yellow world } ", spec).apply_to(&data),
            make_nested_path_expected((14, 18), (19, 25), spec),
        );

        fn make_partial_array_expected(
            goodbye_range: (usize, usize),
            spec: ConnectSpec,
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
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .goodbye not found in object",
                        "path": ["array", 2, "goodbye"],
                        "range": goodbye_range,
                        "spec": spec.to_string(),
                    })),
                ],
            )
        }
        assert_eq!(
            selection!("partial: $.array { hello goodbye }", spec).apply_to(&data),
            make_partial_array_expected((25, 32), spec),
        );
        assert_eq!(
            selection!(" partial : $ . array { hello goodbye } ", spec).apply_to(&data),
            make_partial_array_expected((29, 36), spec),
        );

        assert_eq!(
            selection!("good: array.hello bad: array.smello", spec).apply_to(&data),
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
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .smello not found in object",
                        "path": ["array", 1, "smello"],
                        "range": [29, 35],
                        "spec": spec.to_string(),
                    })),
                ],
            )
        );

        assert_eq!(
            selection!("array { hello smello }", spec).apply_to(&data),
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
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .smello not found in object",
                        "path": ["array", 1, "smello"],
                        "range": [14, 20],
                        "spec": spec.to_string(),
                    })),
                ],
            )
        );

        assert_eq!(
            selection!("$.nested { grouped: { hello smelly world } }", spec).apply_to(&data),
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
                    "spec": spec.to_string(),
                }))],
            )
        );

        assert_eq!(
            selection!("alias: $.nested { grouped: { hello smelly world } }", spec).apply_to(&data),
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
                    "spec": spec.to_string(),
                }))],
            )
        );
    }

    #[rstest]
    #[case::v0_2(ConnectSpec::V0_2)]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_apply_to_nested_arrays(#[case] spec: ConnectSpec) {
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
            spec: ConnectSpec,
        ) -> (Option<JSON>, Vec<ApplyToError>) {
            (
                Some(json!([[0], [1, 1, 1], [2, 2], [], [null, 4, 4, null, 4]])),
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Property .x not found in null",
                        "path": ["arrayOfArrays", 4, 0, "x"],
                        "range": x_range,
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .x not found in null",
                        "path": ["arrayOfArrays", 4, 3, "x"],
                        "range": x_range,
                        "spec": spec.to_string(),
                    })),
                ],
            )
        }
        assert_eq!(
            selection!("arrayOfArrays.x", spec).apply_to(&data),
            make_array_of_arrays_x_expected((14, 15), spec),
        );
        assert_eq!(
            selection!("$.arrayOfArrays.x", spec).apply_to(&data),
            make_array_of_arrays_x_expected((16, 17), spec),
        );

        fn make_array_of_arrays_y_expected(
            y_range: (usize, usize),
            spec: ConnectSpec,
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
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .y not found in object",
                        "path": ["arrayOfArrays", 4, 2, "y"],
                        "range": y_range,
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .y not found in null",
                        "path": ["arrayOfArrays", 4, 3, "y"],
                        "range": y_range,
                        "spec": spec.to_string(),
                    })),
                ],
            )
        }
        assert_eq!(
            selection!("arrayOfArrays.y", spec).apply_to(&data),
            make_array_of_arrays_y_expected((14, 15), spec),
        );
        assert_eq!(
            selection!("$.arrayOfArrays.y", spec).apply_to(&data),
            make_array_of_arrays_y_expected((16, 17), spec),
        );

        assert_eq!(
            selection!("alias: arrayOfArrays { x y }", spec).apply_to(&data),
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
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .y not found in null",
                        "path": ["arrayOfArrays", 4, 0, "y"],
                        "range": [25, 26],
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .y not found in object",
                        "path": ["arrayOfArrays", 4, 2, "y"],
                        "range": [25, 26],
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .x not found in null",
                        "path": ["arrayOfArrays", 4, 3, "x"],
                        "range": [23, 24],
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .y not found in null",
                        "path": ["arrayOfArrays", 4, 3, "y"],
                        "range": [25, 26],
                        "spec": spec.to_string(),
                    })),
                ],
            ),
        );

        fn make_array_of_arrays_x_y_expected(
            x_range: (usize, usize),
            y_range: (usize, usize),
            spec: ConnectSpec,
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
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .y not found in object",
                        "path": ["arrayOfArrays", 4, 2, "y"],
                        "range": y_range,
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        // Reversing the order of "path" and "message" here to make
                        // sure that doesn't affect the deduplication logic.
                        "path": ["arrayOfArrays", 4, 3, "y"],
                        "message": "Property .y not found in null",
                        "range": y_range,
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .x not found in null",
                        "path": ["arrayOfArrays", 4, 0, "x"],
                        "range": x_range,
                        "spec": spec.to_string(),
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .x not found in null",
                        "path": ["arrayOfArrays", 4, 3, "x"],
                        "range": x_range,
                        "spec": spec.to_string(),
                    })),
                ],
            )
        }
        assert_eq!(
            selection!("ys: arrayOfArrays.y xs: arrayOfArrays.x", spec).apply_to(&data),
            make_array_of_arrays_x_y_expected((38, 39), (18, 19), spec),
        );
        assert_eq!(
            selection!("ys: $.arrayOfArrays.y xs: $.arrayOfArrays.x", spec).apply_to(&data),
            make_array_of_arrays_x_y_expected((42, 43), (20, 21), spec),
        );
    }

    #[rstest]
    #[case::v0_2(ConnectSpec::V0_2)]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_apply_to_variable_expressions(#[case] spec: ConnectSpec) {
        let id_object = selection!("id: $", spec).apply_to(&json!(123));
        assert_eq!(id_object, (Some(json!({"id": 123})), vec![]));

        let data = json!({
            "id": 123,
            "name": "Ben",
            "friend_ids": [234, 345, 456]
        });

        assert_eq!(
            selection!("id name friends: friend_ids { id: $ }", spec).apply_to(&data),
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
            selection!("id: $args.id name", spec).apply_with_vars(&data, &vars),
            (
                Some(json!({
                    "id": "id from args",
                    "name": "Ben"
                })),
                vec![],
            ),
        );
        assert_eq!(
            selection!("nested.path { id: $args.id name }", spec).apply_to(&json!({
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
                    "spec": spec.to_string(),
                }))],
            ),
        );
        let mut vars_without_args_id = IndexMap::default();
        vars_without_args_id.insert("$args".to_string(), json!({ "unused": "ignored" }));
        assert_eq!(
            selection!("id: $args.id name", spec).apply_with_vars(&data, &vars_without_args_id),
            (
                Some(json!({
                    "name": "Ben"
                })),
                vec![ApplyToError::from_json(&json!({
                    "message": "Property .id not found in object",
                    "path": ["$args", "id"],
                    "range": [10, 12],
                    "spec": spec.to_string(),
                }))],
            ),
        );

        // A single variable path should not be mapped over an input array.
        assert_eq!(
            selection!("$args.id", spec).apply_with_vars(&json!([1, 2, 3]), &vars),
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
                    ConnectSpec::latest(),
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

    #[rstest]
    #[case::v0_2(ConnectSpec::V0_2)]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_inline_paths_with_subselections(#[case] spec: ConnectSpec) {
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
                "#,
                    spec
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
                "#,
                    spec
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
                "#,
                    spec
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
                "#,
                    spec
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
                "#,
                    spec
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
                "#,
                    spec
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
                "#,
                    spec
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
                "#,
                    spec
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
                "#,
                    spec
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
                "#,
                    spec
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
                "#,
                    spec
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
                "#,
                    spec
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
                "#,
                    spec
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
                "#,
                    spec
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
                "#,
                    spec
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
                "#,
                    spec
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
                "#,
                    spec
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
                        ConnectSpec::latest(),
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
                        ConnectSpec::latest(),
                    ),
                    ApplyToError::new(
                        "Expected object or null, not string".to_string(),
                        vec![],
                        // This is the range of the whole
                        // `choices->first.message { role content }`
                        // subselection.
                        Some(98..137),
                        ConnectSpec::latest(),
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
                        ConnectSpec::latest(),
                    ),
                    ApplyToError::new(
                        "Inlined path produced no value".to_string(),
                        vec![],
                        // This is the range of the whole
                        // `nested.path.nonexistent { name }` path selection.
                        Some(3..35),
                        ConnectSpec::latest(),
                    ),
                ],
            ),
        );

        let valid_inline_path_selection = JSONSelection::named(SubSelection {
            selections: vec![NamedSelection {
                prefix: NamingPrefix::None,
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

    #[rstest]
    #[case::latest(ConnectSpec::V0_2)]
    #[case::next(ConnectSpec::V0_3)]
    fn test_left_associative_path_evaluation(#[case] spec: ConnectSpec) {
        assert_eq!(
            selection!("batch.id->first", spec).apply_to(&json!({
                "batch": [
                    { "id": 1 },
                    { "id": 2 },
                    { "id": 3 },
                ],
            })),
            (Some(json!(1)), vec![]),
        );

        assert_eq!(
            selection!("batch.id->last", spec).apply_to(&json!({
                "batch": [
                    { "id": 1 },
                    { "id": 2 },
                    { "id": 3 },
                ],
            })),
            (Some(json!(3)), vec![]),
        );

        assert_eq!(
            selection!("batch.id->size", spec).apply_to(&json!({
                "batch": [
                    { "id": 1 },
                    { "id": 2 },
                    { "id": 3 },
                ],
            })),
            (Some(json!(3)), vec![]),
        );

        assert_eq!(
            selection!("batch.id->slice(1)->first", spec).apply_to(&json!({
                "batch": [
                    { "id": 1 },
                    { "id": 2 },
                    { "id": 3 },
                ],
            })),
            (Some(json!(2)), vec![]),
        );

        assert_eq!(
            selection!("batch.id->map({ batchId: @ })", spec).apply_to(&json!({
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
            selection!("$batch.id->map({ batchId: @ })", spec).apply_with_vars(
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
            selection!("batch.id->map({ batchId: @ })->first", spec).apply_to(&json!({
                "batch": [
                    { "id": 7 },
                    { "id": 8 },
                    { "id": 9 },
                ],
            })),
            (Some(json!({ "batchId": 7 })), vec![]),
        );

        assert_eq!(
            selection!("batch.id->map({ batchId: @ })->last", spec).apply_to(&json!({
                "batch": [
                    { "id": 7 },
                    { "id": 8 },
                    { "id": 9 },
                ],
            })),
            (Some(json!({ "batchId": 9 })), vec![]),
        );

        assert_eq!(
            selection!("$batch.id->map({ batchId: @ })->first", spec).apply_with_vars(
                &json!({
                    "batch": "ignored",
                }),
                &vars
            ),
            (Some(json!({ "batchId": 4 })), vec![]),
        );

        assert_eq!(
            selection!("$batch.id->map({ batchId: @ })->last", spec).apply_with_vars(
                &json!({
                    "batch": "ignored",
                }),
                &vars
            ),
            (Some(json!({ "batchId": 6 })), vec![]),
        );

        assert_eq!(
            selection!("arrays.as.bs->echo({ echoed: @ })", spec).apply_to(&json!({
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
            selection!("arrays.as.bs->echo({ echoed: @ })", spec).apply_to(&json!({
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
            selection!("batch.id->jsonStringify", spec).apply_to(&json!({
                "batch": [
                    { "id": 1 },
                    { "id": 2 },
                    { "id": 3 },
                ],
            })),
            (Some(json!("[1,2,3]")), vec![]),
        );

        assert_eq!(
            selection!("batch.id->map([@])->echo([@])->jsonStringify", spec).apply_to(&json!({
                "batch": [
                    { "id": 1 },
                    { "id": 2 },
                    { "id": 3 },
                ],
            })),
            (Some(json!("[[[1],[2],[3]]]")), vec![]),
        );

        assert_eq!(
            selection!("batch.id->map([@])->echo([@])->jsonStringify->typeof", spec).apply_to(
                &json!({
                    "batch": [
                        { "id": 1 },
                        { "id": 2 },
                        { "id": 3 },
                    ],
                })
            ),
            (Some(json!("string")), vec![]),
        );
    }

    #[test]
    fn test_left_associative_output_shapes_v0_2() {
        let spec = ConnectSpec::V0_2;

        assert_eq!(
            selection!("$batch.id", spec).shape().pretty_print(),
            "$batch.id"
        );

        assert_eq!(
            selection!("$batch.id->first", spec).shape().pretty_print(),
            "Unknown",
        );

        assert_eq!(
            selection!("$batch.id->last", spec).shape().pretty_print(),
            "Unknown",
        );

        let mut named_shapes = IndexMap::default();
        named_shapes.insert(
            "$batch".to_string(),
            Shape::list(
                Shape::record(
                    {
                        let mut map = Shape::empty_map();
                        map.insert("id".to_string(), Shape::int([]));
                        map
                    },
                    [],
                ),
                [],
            ),
        );

        let root_shape = Shape::name("$root", []);
        let shape_context = ShapeContext::new(SourceId::Other("JSONSelection".into()))
            .with_spec(spec)
            .with_named_shapes(named_shapes);

        let computed_batch_id =
            selection!("$batch.id", spec).compute_output_shape(&shape_context, root_shape.clone());
        assert_eq!(computed_batch_id.pretty_print(), "List<Int>");

        let computed_first = selection!("$batch.id->first", spec)
            .compute_output_shape(&shape_context, root_shape.clone());
        assert_eq!(computed_first.pretty_print(), "Unknown");

        let computed_last = selection!("$batch.id->last", spec)
            .compute_output_shape(&shape_context, root_shape.clone());
        assert_eq!(computed_last.pretty_print(), "Unknown");

        assert_eq!(
            selection!("$batch.id->jsonStringify", spec)
                .shape()
                .pretty_print(),
            "Unknown",
        );

        assert_eq!(
            selection!("$batch.id->map([@])->echo([@])->jsonStringify", spec)
                .shape()
                .pretty_print(),
            "Unknown",
        );

        assert_eq!(
            selection!("$batch.id->map(@)->echo(@)", spec)
                .shape()
                .pretty_print(),
            "Unknown",
        );

        assert_eq!(
            selection!("$batch.id->map(@)->echo([@])", spec)
                .shape()
                .pretty_print(),
            "Unknown",
        );

        assert_eq!(
            selection!("$batch.id->map([@])->echo(@)", spec)
                .shape()
                .pretty_print(),
            "Unknown",
        );

        assert_eq!(
            selection!("$batch.id->map([@])->echo([@])", spec)
                .shape()
                .pretty_print(),
            "Unknown",
        );

        assert_eq!(
            selection!("$batch.id->map([@])->echo([@])", spec)
                .compute_output_shape(&shape_context, root_shape,)
                .pretty_print(),
            "Unknown",
        );
    }

    #[test]
    fn test_left_associative_output_shapes_v0_3() {
        let spec = ConnectSpec::V0_3;

        assert_eq!(
            selection!("$batch.id", spec).shape().pretty_print(),
            "$batch.id"
        );

        assert_eq!(
            selection!("$batch.id->first", spec).shape().pretty_print(),
            "$batch.id.0",
        );

        assert_eq!(
            selection!("$batch.id->last", spec).shape().pretty_print(),
            "$batch.id.*",
        );

        let mut named_shapes = IndexMap::default();
        named_shapes.insert(
            "$batch".to_string(),
            Shape::list(
                Shape::record(
                    {
                        let mut map = Shape::empty_map();
                        map.insert("id".to_string(), Shape::int([]));
                        map
                    },
                    [],
                ),
                [],
            ),
        );

        let root_shape = Shape::name("$root", []);
        let shape_context = ShapeContext::new(SourceId::Other("JSONSelection".into()))
            .with_spec(spec)
            .with_named_shapes(named_shapes.clone());

        let computed_batch_id =
            selection!("$batch.id", spec).compute_output_shape(&shape_context, root_shape.clone());
        assert_eq!(computed_batch_id.pretty_print(), "List<Int>");

        let computed_first = selection!("$batch.id->first", spec)
            .compute_output_shape(&shape_context, root_shape.clone());
        assert_eq!(computed_first.pretty_print(), "One<Int, None>");

        let computed_last = selection!("$batch.id->last", spec)
            .compute_output_shape(&shape_context, root_shape.clone());
        assert_eq!(computed_last.pretty_print(), "One<Int, None>");

        assert_eq!(
            selection!("$batch.id->jsonStringify", spec)
                .shape()
                .pretty_print(),
            "String",
        );

        assert_eq!(
            selection!("$batch.id->map([@])->echo([@])->jsonStringify", spec)
                .shape()
                .pretty_print(),
            "String",
        );

        assert_eq!(
            selection!("$batch.id->map(@)->echo(@)", spec)
                .shape()
                .pretty_print(),
            "List<$batch.id.*>",
        );

        assert_eq!(
            selection!("$batch.id->map(@)->echo([@])", spec)
                .shape()
                .pretty_print(),
            "[List<$batch.id.*>]",
        );

        assert_eq!(
            selection!("$batch.id->map([@])->echo(@)", spec)
                .shape()
                .pretty_print(),
            "List<[$batch.id.*]>",
        );

        assert_eq!(
            selection!("$batch.id->map([@])->echo([@])", spec)
                .shape()
                .pretty_print(),
            "[List<[$batch.id.*]>]",
        );

        assert_eq!(
            selection!("$batch.id->map([@])->echo([@])", spec)
                .compute_output_shape(&shape_context, root_shape,)
                .pretty_print(),
            "[List<[Int]>]",
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

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_optional_key_access_with_existing_property(#[case] spec: ConnectSpec) {
        use serde_json_bytes::json;

        let data = json!({
            "user": {
                "profile": {
                    "name": "Alice"
                }
            }
        });

        let (result, errors) = JSONSelection::parse_with_spec("$.user?.profile.name", spec)
            .unwrap()
            .apply_to(&data);
        assert!(errors.is_empty());
        assert_eq!(result, Some(json!("Alice")));
    }

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_optional_key_access_with_null_value(#[case] spec: ConnectSpec) {
        use serde_json_bytes::json;

        let data_null = json!({
            "user": null
        });

        let (result, errors) = JSONSelection::parse_with_spec("$.user?.profile.name", spec)
            .unwrap()
            .apply_to(&data_null);
        assert!(errors.is_empty());
        assert_eq!(result, None);
    }

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_optional_key_access_on_non_object(#[case] spec: ConnectSpec) {
        use serde_json_bytes::json;

        let data_non_obj = json!({
            "user": "not an object"
        });

        let (result, errors) = JSONSelection::parse_with_spec("$.user?.profile.name", spec)
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

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_optional_key_access_with_missing_property(#[case] spec: ConnectSpec) {
        use serde_json_bytes::json;

        let data = json!({
            "user": {
                "other": "value"
            }
        });

        let (result, errors) = JSONSelection::parse_with_spec("$.user?.profile.name", spec)
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

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_chained_optional_key_access(#[case] spec: ConnectSpec) {
        use serde_json_bytes::json;

        let data = json!({
            "user": {
                "profile": {
                    "name": "Alice"
                }
            }
        });

        let (result, errors) = JSONSelection::parse_with_spec("$.user?.profile?.name", spec)
            .unwrap()
            .apply_to(&data);
        assert!(errors.is_empty());
        assert_eq!(result, Some(json!("Alice")));
    }

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_chained_optional_access_with_null_in_middle(#[case] spec: ConnectSpec) {
        use serde_json_bytes::json;

        let data_partial_null = json!({
            "user": {
                "profile": null
            }
        });

        let (result, errors) = JSONSelection::parse_with_spec("$.user?.profile?.name", spec)
            .unwrap()
            .apply_to(&data_partial_null);
        assert!(errors.is_empty());
        assert_eq!(result, None);
    }

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_optional_method_on_null(#[case] spec: ConnectSpec) {
        use serde_json_bytes::json;

        let data = json!({
            "items": null
        });

        let (result, errors) = JSONSelection::parse_with_spec("$.items?->first", spec)
            .unwrap()
            .apply_to(&data);
        assert!(errors.is_empty());
        assert_eq!(result, None);
    }

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_optional_method_with_valid_method(#[case] spec: ConnectSpec) {
        use serde_json_bytes::json;

        let data = json!({
            "values": [1, 2, 3]
        });

        let (result, errors) = JSONSelection::parse_with_spec("$.values?->first", spec)
            .unwrap()
            .apply_to(&data);
        assert!(errors.is_empty());
        assert_eq!(result, Some(json!(1)));
    }

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_optional_method_with_unknown_method(#[case] spec: ConnectSpec) {
        use serde_json_bytes::json;

        let data = json!({
            "values": [1, 2, 3]
        });

        let (result, errors) = JSONSelection::parse_with_spec("$.values?->length", spec)
            .unwrap()
            .apply_to(&data);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message().contains("Method ->length not found"));
        assert_eq!(result, None);
    }

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_optional_chaining_with_subselection_on_valid_data(#[case] spec: ConnectSpec) {
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

        let (result, errors) = JSONSelection::parse_with_spec("$.user?.profile { name age }", spec)
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

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_optional_chaining_with_subselection_on_null_data(#[case] spec: ConnectSpec) {
        use serde_json_bytes::json;

        let data_null = json!({
            "user": null
        });

        let (result, errors) = JSONSelection::parse_with_spec("$.user?.profile { name age }", spec)
            .unwrap()
            .apply_to(&data_null);
        assert!(errors.is_empty());
        assert_eq!(result, None);
    }

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_mixed_regular_and_optional_chaining_working_case(#[case] spec: ConnectSpec) {
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

        let (result, errors) =
            JSONSelection::parse_with_spec("$.response.data?.user.profile.name", spec)
                .unwrap()
                .apply_to(&data);
        assert!(errors.is_empty());
        assert_eq!(result, Some(json!("Bob")));
    }

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_mixed_regular_and_optional_chaining_with_null(#[case] spec: ConnectSpec) {
        use serde_json_bytes::json;

        let data_null_data = json!({
            "response": {
                "data": null
            }
        });

        let (result, errors) =
            JSONSelection::parse_with_spec("$.response.data?.user.profile.name", spec)
                .unwrap()
                .apply_to(&data_null_data);
        assert!(errors.is_empty());
        assert_eq!(result, None);
    }

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_optional_selection_set_with_valid_data(#[case] spec: ConnectSpec) {
        use serde_json_bytes::json;

        let data = json!({
            "user": {
                "id": 123,
                "name": "Alice"
            }
        });

        let (result, errors) = JSONSelection::parse_with_spec("$.user ?{ id name }", spec)
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

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_optional_selection_set_with_null_data(#[case] spec: ConnectSpec) {
        use serde_json_bytes::json;

        let data = json!({
            "user": null
        });

        let (result, errors) = JSONSelection::parse_with_spec("$.user ?{ id name }", spec)
            .unwrap()
            .apply_to(&data);
        assert_eq!(result, None);
        assert_eq!(errors, vec![]);
    }

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_optional_selection_set_with_missing_property(#[case] spec: ConnectSpec) {
        use serde_json_bytes::json;

        let data = json!({
            "other": "value"
        });

        let (result, errors) = JSONSelection::parse_with_spec("$.user ?{ id name }", spec)
            .unwrap()
            .apply_to(&data);
        assert_eq!(result, None);
        assert_eq!(errors.len(), 0);
    }

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_optional_selection_set_with_non_object(#[case] spec: ConnectSpec) {
        use serde_json_bytes::json;

        let data = json!({
            "user": "not an object"
        });

        let (result, errors) = JSONSelection::parse_with_spec("$.user ?{ id name }", spec)
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
    fn test_optional_field_selections() {
        let spec = ConnectSpec::V0_3;
        let author_selection = selection!("author? { age middleName? }", spec);
        assert_debug_snapshot!(author_selection);
        assert_eq!(
            author_selection.pretty_print(),
            "author? { age middleName? }",
        );
        assert_eq!(
            author_selection.shape().pretty_print(),
            "{ author: One<{ age: $root.*.author.*.age, middleName: One<$root.*.author.*.middleName, None> }, None> }",
        );
    }

    #[cfg(test)]
    mod spread {
        use serde_json_bytes::Value as JSON;
        use serde_json_bytes::json;
        use shape::Shape;
        use shape::location::SourceId;

        use crate::connectors::ConnectSpec;
        use crate::connectors::json_selection::ShapeContext;

        #[derive(Debug)]
        pub(super) struct SetupItems {
            pub data: JSON,
            pub shape_context: ShapeContext,
            pub root_shape: Shape,
        }

        pub(super) fn setup(spec: ConnectSpec) -> SetupItems {
            let a_b_data = json!({
                "a": { "phonetic": "ay" },
                "b": { "phonetic": "bee" },
            });

            let a_b_data_shape = Shape::from_json_bytes(&a_b_data);

            let shape_context = ShapeContext::new(SourceId::Other("JSONSelection".into()))
                .with_spec(spec)
                .with_named_shapes([("$root".to_string(), a_b_data_shape)]);

            let root_shape = shape_context.named_shapes().get("$root").unwrap().clone();

            SetupItems {
                data: a_b_data,
                shape_context,
                root_shape,
            }
        }
    }

    #[test]
    fn test_spread_syntax_spread_a() {
        let spec = ConnectSpec::V0_3;
        let spread::SetupItems {
            data: a_b_data,
            shape_context,
            root_shape,
        } = spread::setup(spec);

        let spread_a = selection!("...a", spec);
        assert_eq!(
            spread_a.apply_to(&a_b_data),
            (Some(json!({"phonetic": "ay"})), vec![]),
        );
        assert_eq!(spread_a.shape().pretty_print(), "$root.*.a",);
        assert_eq!(
            spread_a
                .compute_output_shape(&shape_context, root_shape)
                .pretty_print(),
            "{ phonetic: \"ay\" }",
        );
    }

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_nested_optional_selection_sets(#[case] spec: ConnectSpec) {
        use serde_json_bytes::json;

        let data = json!({
            "user": {
                "profile": {
                    "name": "Alice",
                    "email": "alice@example.com"
                }
            }
        });

        let (result, errors) =
            JSONSelection::parse_with_spec("$.user.profile ?{ name email }", spec)
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

        let (result, errors) =
            JSONSelection::parse_with_spec("$.user.profile ?{ name email }", spec)
                .unwrap()
                .apply_to(&data_with_null_profile);
        assert_eq!(result, None);
        assert_eq!(errors, vec![]);
    }

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_mixed_optional_selection_and_optional_chaining(#[case] spec: ConnectSpec) {
        use serde_json_bytes::json;

        let data = json!({
            "user": {
                "id": 123,
                "profile": null
            }
        });

        let (result, errors) =
            JSONSelection::parse_with_spec("$.user ?{ id profileName: profile?.name }", spec)
                .unwrap()
                .apply_to(&data);
        assert_eq!(
            result,
            Some(json!({
                "id": 123
            }))
        );
        assert_eq!(errors, vec![]);

        // Test with missing user
        let data_no_user = json!({
            "other": "value"
        });

        let (result, errors) =
            JSONSelection::parse_with_spec("$.user ?{ id profileName: profile?.name }", spec)
                .unwrap()
                .apply_to(&data_no_user);
        assert_eq!(result, None);
        assert_eq!(errors.len(), 0);
    }

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_optional_selection_set_parsing(#[case] spec: ConnectSpec) {
        // Test that the parser correctly handles optional selection sets
        let selection = JSONSelection::parse_with_spec("$.user? { id name }", spec).unwrap();
        assert_eq!(selection.pretty_print(), "$.user? { id name }");

        // Test with nested optional selection sets
        let selection = JSONSelection::parse_with_spec("$.user.profile? { name }", spec).unwrap();
        assert_eq!(selection.pretty_print(), "$.user.profile? { name }");

        // Test mixed with regular selection sets
        let selection =
            JSONSelection::parse_with_spec("$.user? { id profile { name } }", spec).unwrap();
        assert_eq!(selection.pretty_print(), "$.user? { id profile { name } }");
    }

    #[rstest]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_optional_selection_set_with_arrays(#[case] spec: ConnectSpec) {
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

        let (result, errors) = JSONSelection::parse_with_spec("$.users ?{ id name }", spec)
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

    #[test]
    fn test_spread_syntax_a_spread_b() {
        let spec = ConnectSpec::V0_3;
        let spread::SetupItems {
            data: a_b_data,
            shape_context,
            root_shape,
        } = spread::setup(spec);

        let a_spread_b = selection!("a...b", spec);
        assert_eq!(
            a_spread_b.apply_to(&a_b_data),
            (
                Some(json!({"a": { "phonetic": "ay" }, "phonetic": "bee" })),
                vec![]
            ),
        );
        assert_eq!(
            a_spread_b.shape().pretty_print(),
            "All<$root.*.b, { a: $root.*.a }>",
        );
        assert_eq!(
            a_spread_b
                .compute_output_shape(&shape_context, root_shape)
                .pretty_print(),
            "{ a: { phonetic: \"ay\" }, phonetic: \"bee\" }",
        );
    }

    #[test]
    fn test_spread_syntax_spread_a_b() {
        let spec = ConnectSpec::V0_3;
        let spread::SetupItems {
            data: a_b_data,
            shape_context,
            root_shape,
        } = spread::setup(spec);

        let spread_a_b = selection!("...a b", spec);
        assert_eq!(
            spread_a_b.apply_to(&a_b_data),
            (
                Some(json!({"phonetic": "ay", "b": { "phonetic": "bee" }})),
                vec![]
            ),
        );
        assert_eq!(
            spread_a_b.shape().pretty_print(),
            "All<$root.*.a, { b: $root.*.b }>",
        );
        assert_eq!(
            spread_a_b
                .compute_output_shape(&shape_context, root_shape)
                .pretty_print(),
            "{ b: { phonetic: \"bee\" }, phonetic: \"ay\" }",
        );
    }

    #[test]
    fn test_spread_match_none() {
        let spec = ConnectSpec::V0_3;

        let sel = selection!(
            "before ...condition->match([true, { matched: true }]) after",
            spec
        );
        assert_eq!(
            sel.shape().pretty_print(),
            "One<{ after: $root.*.after, before: $root.*.before, matched: true }, { after: $root.*.after, before: $root.*.before }>",
        );

        assert_eq!(
            sel.apply_to(&json!({
                "before": "before value",
                "after": "after value",
                "condition": true,
            })),
            (
                Some(json!({
                    "before": "before value",
                    "after": "after value",
                    "matched": true,
                })),
                vec![],
            ),
        );

        assert_eq!(
            sel.apply_to(&json!({
                "before": "before value",
                "after": "after value",
                "condition": false,
            })),
            (
                Some(json!({
                    "before": "before value",
                    "after": "after value",
                })),
                vec![
                    ApplyToError::new(
                        "Method ->match did not match any [candidate, value] pair".to_string(),
                        vec![json!("condition"), json!("->match")],
                        Some(21..53),
                        spec,
                    ),
                    ApplyToError::new(
                        "Inlined path produced no value".to_string(),
                        vec![],
                        Some(10..53),
                        spec,
                    )
                ],
            ),
        );
    }

    #[cfg(test)]
    mod spread_with_match {
        use crate::connectors::ConnectSpec;
        use crate::connectors::JSONSelection;
        use crate::selection;

        pub(super) fn get_selection(spec: ConnectSpec) -> JSONSelection {
            let sel = selection!(
                r#"
                upc
                ... type->match(
                    ["book", {
                        __typename: "Book",
                        title: title,
                        author: { name: author.name },
                    }],
                    ["movie", {
                        __typename: "Movie",
                        title: title,
                        director: director.name,
                    }],
                    ["magazine", {
                        __typename: "Magazine",
                        title: title,
                        editor: editor.name,
                    }],
                    ["dummy", {}],
                    [@, null],
                )
                "#,
                spec
            );

            assert_eq!(
                sel.shape().pretty_print(),
                // An upcoming Shape library update should improve the readability
                // of this pretty printing considerably.
                "One<{ __typename: \"Book\", author: { name: $root.*.author.name }, title: $root.*.title, upc: $root.*.upc }, { __typename: \"Movie\", director: $root.*.director.name, title: $root.*.title, upc: $root.*.upc }, { __typename: \"Magazine\", editor: $root.*.editor.name, title: $root.*.title, upc: $root.*.upc }, { upc: $root.*.upc }, null>"
            );

            sel
        }
    }

    #[test]
    fn test_spread_with_match_book() {
        let spec = ConnectSpec::V0_3;
        let sel = spread_with_match::get_selection(spec);

        let book_data = json!({
            "upc": "1234567890",
            "type": "book",
            "title": "The Great Gatsby",
            "author": { "name": "F. Scott Fitzgerald" },
        });
        assert_eq!(
            sel.apply_to(&book_data),
            (
                Some(json!({
                    "__typename": "Book",
                    "upc": "1234567890",
                    "title": "The Great Gatsby",
                    "author": { "name": "F. Scott Fitzgerald" },
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn test_spread_with_match_movie() {
        let spec = ConnectSpec::V0_3;
        let sel = spread_with_match::get_selection(spec);

        let movie_data = json!({
            "upc": "0987654321",
            "type": "movie",
            "title": "Inception",
            "director": { "name": "Christopher Nolan" },
        });
        assert_eq!(
            sel.apply_to(&movie_data),
            (
                Some(json!({
                    "__typename": "Movie",
                    "upc": "0987654321",
                    "title": "Inception",
                    "director": "Christopher Nolan",
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn test_spread_with_match_magazine() {
        let spec = ConnectSpec::V0_3;
        let sel = spread_with_match::get_selection(spec);

        let magazine_data = json!({
            "upc": "1122334455",
            "type": "magazine",
            "title": "National Geographic",
            "editor": { "name": "Susan Goldberg" },
        });
        assert_eq!(
            sel.apply_to(&magazine_data),
            (
                Some(json!({
                    "__typename": "Magazine",
                    "upc": "1122334455",
                    "title": "National Geographic",
                    "editor": "Susan Goldberg",
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn test_spread_with_match_dummy() {
        let spec = ConnectSpec::V0_3;
        let sel = spread_with_match::get_selection(spec);

        let dummy_data = json!({
            "upc": "5566778899",
            "type": "dummy",
        });
        assert_eq!(
            sel.apply_to(&dummy_data),
            (
                Some(json!({
                    "upc": "5566778899",
                })),
                vec![],
            ),
        );
    }

    #[test]
    fn test_spread_with_match_unknown() {
        let spec = ConnectSpec::V0_3;
        let sel = spread_with_match::get_selection(spec);

        let unknown_data = json!({
            "upc": "9988776655",
            "type": "music",
            "title": "The White Stripes",
            "artist": { "name": "Jack White" },
        });
        assert_eq!(sel.apply_to(&unknown_data), (Some(json!(null)), vec![]));
    }

    #[test]
    fn test_spread_null() {
        let spec = ConnectSpec::V0_3;
        assert_eq!(
            selection!("...$(null)", spec).apply_to(&json!({ "ignored": "data" })),
            (Some(json!(null)), vec![]),
        );
        assert_eq!(
            selection!("ignored ...$(null)", spec).apply_to(&json!({ "ignored": "data" })),
            (Some(json!(null)), vec![]),
        );
        assert_eq!(
            selection!("...$(null) ignored", spec).apply_to(&json!({ "ignored": "data" })),
            (Some(json!(null)), vec![]),
        );
        assert_eq!(
            selection!("group: { a ...b }", spec).apply_to(&json!({ "a": "ay", "b": null })),
            (Some(json!({ "group": null })), vec![]),
        );
    }

    #[test]
    fn test_spread_missing() {
        let spec = ConnectSpec::V0_3;

        assert_eq!(
            selection!("a ...missing z", spec).apply_to(&json!({ "a": "ay", "z": "zee" })),
            (
                Some(json!({
                    "a": "ay",
                    "z": "zee",
                })),
                vec![
                    ApplyToError::new(
                        "Property .missing not found in object".to_string(),
                        vec![json!("missing")],
                        Some(5..12),
                        spec,
                    ),
                    ApplyToError::new(
                        "Inlined path produced no value".to_string(),
                        vec![],
                        Some(5..12),
                        spec,
                    ),
                ],
            ),
        );

        assert_eq!(
            selection!("a ...$(missing) z", spec).apply_to(&json!({ "a": "ay", "z": "zee" })),
            (
                Some(json!({
                    "a": "ay",
                    "z": "zee",
                })),
                vec![
                    ApplyToError::new(
                        "Property .missing not found in object".to_string(),
                        vec![json!("missing")],
                        Some(7..14),
                        spec,
                    ),
                    ApplyToError::new(
                        "Inlined path produced no value".to_string(),
                        vec![],
                        Some(5..15),
                        spec,
                    ),
                ],
            ),
        );
    }

    #[test]
    fn test_spread_invalid_numbers() {
        let spec = ConnectSpec::V0_3;

        assert_eq!(
            selection!("...invalid", spec).apply_to(&json!({ "invalid": 123 })),
            (
                Some(json!({})),
                vec![ApplyToError::new(
                    "Expected object or null, not number".to_string(),
                    vec![],
                    Some(3..10),
                    spec,
                )],
            ),
        );

        assert_eq!(
            selection!(" ... $( invalid ) ", spec).apply_to(&json!({ "invalid": 234 })),
            (
                Some(json!({})),
                vec![ApplyToError::new(
                    "Expected object or null, not number".to_string(),
                    vec![],
                    Some(5..17),
                    spec,
                )],
            ),
        );
    }

    #[test]
    fn test_spread_invalid_bools() {
        let spec = ConnectSpec::V0_3;

        assert_eq!(
            selection!("...invalid", spec).apply_to(&json!({ "invalid": true })),
            (
                Some(json!({})),
                vec![ApplyToError::new(
                    "Expected object or null, not boolean".to_string(),
                    vec![],
                    Some(3..10),
                    spec,
                )],
            ),
        );

        assert_eq!(
            selection!("...$(invalid)", spec).apply_to(&json!({ "invalid": false })),
            (
                Some(json!({})),
                vec![ApplyToError::new(
                    "Expected object or null, not boolean".to_string(),
                    vec![],
                    Some(3..13),
                    spec,
                )],
            ),
        );
    }

    #[test]
    fn test_spread_invalid_strings() {
        let spec = ConnectSpec::V0_3;

        assert_eq!(
            selection!("...invalid", spec).apply_to(&json!({ "invalid": "string" })),
            (
                Some(json!({})),
                vec![ApplyToError::new(
                    "Expected object or null, not string".to_string(),
                    vec![],
                    Some(3..10),
                    spec,
                )],
            ),
        );

        assert_eq!(
            selection!("...$(invalid)", spec).apply_to(&json!({ "invalid": "string" })),
            (
                Some(json!({})),
                vec![ApplyToError::new(
                    "Expected object or null, not string".to_string(),
                    vec![],
                    Some(3..13),
                    spec,
                )],
            ),
        );
    }

    #[test]
    fn test_spread_invalid_arrays() {
        let spec = ConnectSpec::V0_3;

        // The ... operator only works for objects for now, as it spreads their
        // keys into some larger object. We may support array spreading in the
        // future, but it will probably work somewhat differently (it may be
        // available only within literal expressions, for example).
        assert_eq!(
            selection!("...invalid", spec).apply_to(&json!({ "invalid": [1, 2, 3] })),
            (
                Some(json!({})),
                vec![ApplyToError::new(
                    "Expected object or null, not array".to_string(),
                    vec![],
                    Some(3..10),
                    spec,
                )],
            ),
        );

        assert_eq!(
            selection!("...$(invalid)", spec).apply_to(&json!({ "invalid": [] })),
            (
                Some(json!({})),
                vec![ApplyToError::new(
                    "Expected object or null, not array".to_string(),
                    vec![],
                    Some(3..13),
                    spec,
                )],
            ),
        );
    }

    #[test]
    fn test_spread_output_shapes() {
        let spec = ConnectSpec::V0_3;

        assert_eq!(selection!("...a", spec).shape().pretty_print(), "$root.*.a");
        assert_eq!(
            selection!("...$(a)", spec).shape().pretty_print(),
            "$root.*.a",
        );

        assert_eq!(
            selection!("a ...b", spec).shape().pretty_print(),
            "All<$root.*.b, { a: $root.*.a }>",
        );
        assert_eq!(
            selection!("a ...$(b)", spec).shape().pretty_print(),
            "All<$root.*.b, { a: $root.*.a }>",
        );

        assert_eq!(
            selection!("a ...b c", spec).shape().pretty_print(),
            "All<$root.*.b, { a: $root.*.a, c: $root.*.c }>",
        );
        assert_eq!(
            selection!("a ...$(b) c", spec).shape().pretty_print(),
            "All<$root.*.b, { a: $root.*.a, c: $root.*.c }>",
        );
    }

    #[test]
    fn null_coalescing_should_return_left_when_left_not_null() {
        let spec = ConnectSpec::V0_3;
        assert_eq!(
            selection!("$('Foo' ?? 'Bar')", spec).apply_to(&json!({})),
            (Some(json!("Foo")), vec![]),
        );
    }

    #[test]
    fn null_coalescing_should_return_right_when_left_is_null() {
        let spec = ConnectSpec::V0_3;
        assert_eq!(
            selection!("$(null ?? 'Bar')", spec).apply_to(&json!({})),
            (Some(json!("Bar")), vec![]),
        );
    }

    #[test]
    fn none_coalescing_should_return_left_when_left_not_none() {
        let spec = ConnectSpec::V0_3;
        assert_eq!(
            selection!("$('Foo' ?! 'Bar')", spec).apply_to(&json!({})),
            (Some(json!("Foo")), vec![]),
        );
    }

    #[test]
    fn none_coalescing_should_preserve_null_when_left_is_null() {
        let spec = ConnectSpec::V0_3;
        assert_eq!(
            selection!("$(null ?! 'Bar')", spec).apply_to(&json!({})),
            (Some(json!(null)), vec![]),
        );
    }

    #[test]
    fn nullish_coalescing_should_return_final_null() {
        let spec = ConnectSpec::V0_3;
        assert_eq!(
            selection!("$(missing ?? null)", spec).apply_to(&json!({})),
            (Some(json!(null)), vec![]),
        );
        assert_eq!(
            selection!("$(missing ?! null)", spec).apply_to(&json!({})),
            (Some(json!(null)), vec![]),
        );
    }

    #[test]
    fn nullish_coalescing_should_return_final_none() {
        let spec = ConnectSpec::V0_3;
        assert_eq!(
            selection!("$(missing ?? also_missing)", spec).apply_to(&json!({})),
            (
                None,
                vec![
                    ApplyToError::new(
                        "Property .missing not found in object".to_string(),
                        vec![json!("missing")],
                        Some(2..9),
                        spec,
                    ),
                    ApplyToError::new(
                        "Property .also_missing not found in object".to_string(),
                        vec![json!("also_missing")],
                        Some(13..25),
                        spec,
                    ),
                ]
            ),
        );
        assert_eq!(
            selection!("maybe: $(missing ?! also_missing)", spec).apply_to(&json!({})),
            (
                Some(json!({})),
                vec![
                    ApplyToError::new(
                        "Property .missing not found in object".to_string(),
                        vec![json!("missing")],
                        Some(9..16),
                        spec,
                    ),
                    ApplyToError::new(
                        "Property .also_missing not found in object".to_string(),
                        vec![json!("also_missing")],
                        Some(20..32),
                        spec,
                    ),
                ]
            ),
        );
    }

    #[test]
    fn coalescing_operators_should_return_earlier_values_if_later_missing() {
        let spec = ConnectSpec::V0_3;
        assert_eq!(
            selection!("$(1234 ?? missing)", spec).apply_to(&json!({})),
            (Some(json!(1234)), vec![]),
        );
        assert_eq!(
            selection!("$(item ?? missing)", spec).apply_to(&json!({ "item": 1234 })),
            (Some(json!(1234)), vec![]),
        );
        assert_eq!(
            selection!("$(item ?? missing)", spec).apply_to(&json!({ "item": null })),
            (
                None,
                vec![ApplyToError::new(
                    "Property .missing not found in object".to_string(),
                    vec![json!("missing")],
                    Some(10..17),
                    spec,
                )]
            ),
        );
        assert_eq!(
            selection!("$(null ?! missing)", spec).apply_to(&json!({})),
            (Some(json!(null)), vec![]),
        );
        assert_eq!(
            selection!("$(item ?! missing)", spec).apply_to(&json!({ "item": null })),
            (Some(json!(null)), vec![]),
        );
    }

    #[test]
    fn null_coalescing_should_chain_left_to_right_when_multiple_nulls() {
        // TODO: TEST HERE
        let spec = ConnectSpec::V0_3;
        assert_eq!(
            selection!("$(null ?? null ?? 'Bar')", spec).apply_to(&json!({})),
            (Some(json!("Bar")), vec![]),
        );
    }

    #[test]
    fn null_coalescing_should_stop_at_first_non_null_when_chaining() {
        let spec = ConnectSpec::V0_3;
        assert_eq!(
            selection!("$('Foo' ?? null ?? 'Bar')", spec).apply_to(&json!({})),
            (Some(json!("Foo")), vec![]),
        );
    }

    #[test]
    fn null_coalescing_should_fallback_when_field_is_null() {
        let spec = ConnectSpec::V0_3;
        let data = json!({"field1": null, "field2": "value2"});
        assert_eq!(
            selection!("$($.field1 ?? $.field2)", spec).apply_to(&data),
            (Some(json!("value2")), vec![]),
        );
    }

    #[test]
    fn null_coalescing_should_use_literal_fallback_when_all_fields_null() {
        let spec = ConnectSpec::V0_3;
        let data = json!({"field1": null, "field3": null});
        assert_eq!(
            selection!("$($.field1 ?? $.field3 ?? 'fallback')", spec).apply_to(&data),
            (Some(json!("fallback")), vec![]),
        );
    }

    #[test]
    fn none_coalescing_should_preserve_null_field() {
        let spec = ConnectSpec::V0_3;
        let data = json!({"nullField": null});
        assert_eq!(
            selection!("$($.nullField ?! 'fallback')", spec).apply_to(&data),
            (Some(json!(null)), vec![]),
        );
    }

    #[test]
    fn none_coalescing_should_replace_missing_field() {
        let spec = ConnectSpec::V0_3;
        let data = json!({"nullField": null});
        assert_eq!(
            selection!("$($.missingField ?! 'fallback')", spec).apply_to(&data),
            (Some(json!("fallback")), vec![]),
        );
    }

    #[test]
    fn null_coalescing_should_replace_null_field() {
        let spec = ConnectSpec::V0_3;
        let data = json!({"nullField": null});
        assert_eq!(
            selection!("$($.nullField ?? 'fallback')", spec).apply_to(&data),
            (Some(json!("fallback")), vec![]),
        );
    }

    #[test]
    fn null_coalescing_should_replace_missing_field() {
        let spec = ConnectSpec::V0_3;
        let data = json!({"nullField": null});
        assert_eq!(
            selection!("$($.missingField ?? 'fallback')", spec).apply_to(&data),
            (Some(json!("fallback")), vec![]),
        );
    }

    #[test]
    fn null_coalescing_should_preserve_number_type() {
        let spec = ConnectSpec::V0_3;
        assert_eq!(
            selection!("$(null ?? 42)", spec).apply_to(&json!({})),
            (Some(json!(42)), vec![]),
        );
    }

    #[test]
    fn null_coalescing_should_preserve_boolean_type() {
        let spec = ConnectSpec::V0_3;
        assert_eq!(
            selection!("$(null ?? true)", spec).apply_to(&json!({})),
            (Some(json!(true)), vec![]),
        );
    }

    #[test]
    fn null_coalescing_should_preserve_object_type() {
        let spec = ConnectSpec::V0_3;
        assert_eq!(
            selection!("$(null ?? {'key': 'value'})", spec).apply_to(&json!({})),
            (Some(json!({"key": "value"})), vec![]),
        );
    }

    #[test]
    fn null_coalescing_should_preserve_array_type() {
        let spec = ConnectSpec::V0_3;
        assert_eq!(
            selection!("$(null ?? [1, 2, 3])", spec).apply_to(&json!({})),
            (Some(json!([1, 2, 3])), vec![]),
        );
    }

    #[test]
    fn null_coalescing_should_fallback_when_null_used_as_method_arg() {
        let spec = ConnectSpec::V0_3;
        assert_eq!(
            selection!("$.a->add(b ?? c)", spec).apply_to(&json!({"a": 5, "b": null, "c": 5})),
            (Some(json!(10)), vec![]),
        );
    }

    #[test]
    fn null_coalescing_should_fallback_when_none_used_as_method_arg() {
        let spec = ConnectSpec::V0_3;
        assert_eq!(
            selection!("$.a->add(missing ?? c)", spec)
                .apply_to(&json!({"a": 5, "b": null, "c": 5})),
            (Some(json!(10)), vec![]),
        );
    }

    #[test]
    fn null_coalescing_should_not_fallback_when_not_null_used_as_method_arg() {
        let spec = ConnectSpec::V0_3;
        assert_eq!(
            selection!("$.a->add(b ?? c)", spec).apply_to(&json!({"a": 5, "b": 3, "c": 5})),
            (Some(json!(8)), vec![]),
        );
    }

    #[test]
    fn null_coalescing_should_allow_multiple_method_args() {
        let spec = ConnectSpec::V0_3;
        let add_selection = selection!("a->add(b ?? c, missing ?! c)", spec);
        assert_eq!(
            add_selection.apply_to(&json!({ "a": 5, "b": 3, "c": 7 })),
            (Some(json!(15)), vec![]),
        );
        assert_eq!(
            add_selection.apply_to(&json!({ "a": 5, "b": null, "c": 7 })),
            (Some(json!(19)), vec![]),
        );
    }

    #[test]
    fn none_coalescing_should_allow_defaulting_match() {
        let spec = ConnectSpec::V0_3;

        assert_eq!(
            selection!("a ...b->match(['match', { b: 'world' }])", spec)
                .apply_to(&json!({ "a": "hello", "b": "match" })),
            (Some(json!({ "a": "hello", "b": "world" })), vec![]),
        );

        assert_eq!(
            selection!("a ...$(b->match(['match', { b: 'world' }]) ?? {})", spec)
                .apply_to(&json!({ "a": "hello", "b": "match" })),
            (Some(json!({ "a": "hello", "b": "world" })), vec![]),
        );

        assert_eq!(
            selection!("a ...$(b->match(['match', { b: 'world' }]) ?? {})", spec)
                .apply_to(&json!({ "a": "hello", "b": "bogus" })),
            (Some(json!({ "a": "hello" })), vec![]),
        );

        assert_eq!(
            selection!("a ...$(b->match(['match', { b: 'world' }]) ?! null)", spec)
                .apply_to(&json!({ "a": "hello", "b": "bogus" })),
            (Some(json!(null)), vec![]),
        );
    }
}
