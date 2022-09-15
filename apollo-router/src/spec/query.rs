//! Query processing.
//!
//! Parsing, formatting and manipulation of queries.

use std::collections::HashMap;
use std::collections::HashSet;

use apollo_parser::ast;
use derivative::Derivative;
use graphql::Error;
use serde_json_bytes::ByteString;
use tracing::level_filters::LevelFilter;

use crate::error::FetchError;
use crate::graphql::Request;
use crate::graphql::Response;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::json_ext::Value;
use crate::query_planner::fetch::OperationKind;
use crate::*;

pub(crate) const TYPENAME: &str = "__typename";

/// A GraphQL query.
#[derive(Debug, Derivative, Default)]
#[derivative(PartialEq, Hash, Eq)]
pub(crate) struct Query {
    string: String,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    fragments: Fragments,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) operations: Vec<Operation>,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) subselections: HashMap<(Option<Path>, String), Query>,
}

impl Query {
    /// Re-format the response value to match this query.
    ///
    /// This will discard unrequested fields and re-order the output to match the order of the
    /// query.
    #[tracing::instrument(skip_all, level = "trace")]
    pub(crate) fn format_response(
        &self,
        response: &mut Response,
        operation_name: Option<&str>,
        variables: Object,
        schema: &Schema,
    ) {
        let data = std::mem::take(&mut response.data);
        if let Some(Value::Object(mut input)) = data {
            let operation = match operation_name {
                Some(name) => self
                    .operations
                    .iter()
                    // we should have an error if the only operation is anonymous but the query specifies a name
                    .find(|op| op.name.is_some() && op.name.as_deref().unwrap() == name),
                None => self.operations.get(0),
            };
            if let Some(subselection) = &response.subselection {
                // Get subselection from hashmap
                match self.subselections.get(&(
                    //FIXME: we should not have optional paths at all in the subselections map
                    response.path.clone().or_else(|| Some(Path::default())),
                    subselection.clone(),
                )) {
                    Some(subselection_query) => {
                        let mut output = Object::default();
                        let operation = &subselection_query.operations[0];
                        let mut parameters = FormatParameters {
                            variables: &variables,
                            schema,
                            errors: Vec::new(),
                        };
                        response.data = Some(
                            match self.apply_root_selection_set(
                                operation,
                                &mut parameters,
                                &mut input,
                                &mut output,
                                &mut Path::default(),
                            ) {
                                Ok(()) => output.into(),
                                Err(InvalidValue) => Value::Null,
                            },
                        );

                        response.errors.extend(parameters.errors.into_iter());

                        return;
                    }
                    None => failfast_debug!("can't find subselection for {:?}", subselection),
                }
            } else if let Some(operation) = operation {
                let mut output = Object::default();

                let all_variables = if operation.variables.is_empty() {
                    variables
                } else {
                    operation
                        .variables
                        .iter()
                        .filter_map(|(k, (_, opt))| opt.as_ref().map(|v| (k, v)))
                        .chain(variables.iter())
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                };

                let mut parameters = FormatParameters {
                    variables: &all_variables,
                    schema,
                    errors: Vec::new(),
                };

                response.data = Some(
                    match self.apply_root_selection_set(
                        operation,
                        &mut parameters,
                        &mut input,
                        &mut output,
                        &mut Path::default(),
                    ) {
                        Ok(()) => output.into(),
                        Err(InvalidValue) => Value::Null,
                    },
                );
                response.errors.extend(parameters.errors.into_iter());

                return;
            } else {
                failfast_debug!("can't find operation for {:?}", operation_name);
            }
        } else {
            failfast_debug!("invalid type for data in response. data: {:#?}", data);
        }

        response.data = Some(Value::default());
    }

    pub(crate) fn parse(
        query: impl Into<String>,
        schema: &Schema,
        configuration: &Configuration,
    ) -> Result<Self, SpecError> {
        let string = query.into();

        let parser = apollo_parser::Parser::with_recursion_limit(
            string.as_str(),
            configuration.server.experimental_parser_recursion_limit,
        );
        let tree = parser.parse();

        // Trace log recursion limit data
        let recursion_limit = tree.recursion_limit();
        tracing::trace!(?recursion_limit, "recursion limit data");

        let errors = tree
            .errors()
            .map(|err| format!("{:?}", err))
            .collect::<Vec<_>>();

        if !errors.is_empty() {
            let errors = errors.join(", ");
            failfast_debug!("parsing error(s): {}", errors);
            return Err(SpecError::ParsingError(errors));
        }

        let document = tree.document();
        let fragments = Fragments::from_ast(&document, schema)?;

        let operations: Vec<Operation> = document
            .definitions()
            .filter_map(|definition| {
                if let ast::Definition::OperationDefinition(operation) = definition {
                    Some(operation)
                } else {
                    None
                }
            })
            .map(|operation| Operation::from_ast(operation, schema))
            .collect::<Result<Vec<_>, SpecError>>()?;

        Ok(Query {
            string,
            fragments,
            operations,
            subselections: HashMap::new(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn format_value(
        &self,
        parameters: &mut FormatParameters,
        field_type: &FieldType,
        input: &mut Value,
        output: &mut Value,
        path: &mut Path,
        parent_type: &FieldType,
        selection_set: &[Selection],
    ) -> Result<(), InvalidValue> {
        // for every type, if we have an invalid value, we will replace it with null
        // and return Ok(()), because values are optional by default
        match field_type {
            // for non null types, we validate with the inner type, then if we get an InvalidValue
            // we set it to null and immediately return an error instead of Ok(()), because we
            // want the error to go up until the next nullable parent
            FieldType::NonNull(inner_type) => {
                match self.format_value(
                    parameters,
                    inner_type,
                    input,
                    output,
                    path,
                    field_type,
                    selection_set,
                ) {
                    Err(_) => Err(InvalidValue),
                    Ok(_) => {
                        if output.is_null() {
                            let message = match path.last() {
                                Some(PathElement::Key(k)) => format!(
                                    "Cannot return null for non-nullable field {parent_type}.{}",
                                   k
                                ),
                                Some(PathElement::Index(i)) => format!(
                                    "Cannot return null for non-nullable array element of type {inner_type} at index {}",
                                   i
                                ),
                                _ => todo!(),
                            };
                            parameters.errors.push(Error {
                                message,
                                path: Some(path.clone()),
                                ..Error::default()
                            });

                            Err(InvalidValue)
                        } else {
                            Ok(())
                        }
                    }
                }
            }

            // if the list contains nonnullable types, we will receive a Err(InvalidValue)
            // and should replace the entire list with null
            // if the types are nullable, the inner call to filter_errors will take care
            // of setting the current entry to null
            FieldType::List(inner_type) => match input {
                Value::Array(input_array) => {
                    if output.is_null() {
                        *output = Value::Array(
                            std::iter::repeat(Value::Null)
                                .take(input_array.len())
                                .collect(),
                        );
                    }
                    let output_array = output.as_array_mut().ok_or(InvalidValue)?;
                    match input_array
                        .iter_mut()
                        .enumerate()
                        .try_for_each(|(i, element)| {
                            path.push(PathElement::Index(i));
                            let res = self.format_value(
                                parameters,
                                inner_type,
                                element,
                                &mut output_array[i],
                                path,
                                field_type,
                                selection_set,
                            );
                            path.pop();
                            res
                        }) {
                        Err(InvalidValue) => {
                            *output = Value::Null;
                            Ok(())
                        }
                        Ok(()) => Ok(()),
                    }
                }
                _ => Ok(()),
            },

            FieldType::Named(type_name) | FieldType::Introspection(type_name) => {
                // we cannot know about the expected format of custom scalars
                // so we must pass them directly to the client
                if parameters.schema.custom_scalars.contains(type_name) {
                    *output = input.clone();
                    return Ok(());
                } else if let Some(enum_type) = parameters.schema.enums.get(type_name) {
                    return match input.as_str() {
                        Some(s) => {
                            if enum_type.contains(s) {
                                *output = input.clone();
                                Ok(())
                            } else {
                                *output = Value::Null;
                                Ok(())
                            }
                        }
                        None => {
                            *output = Value::Null;
                            Ok(())
                        }
                    };
                }

                match input {
                    Value::Object(ref mut input_object) => {
                        if let Some(input_type) =
                            input_object.get(TYPENAME).and_then(|val| val.as_str())
                        {
                            if !parameters.schema.object_types.contains_key(input_type) {
                                *output = Value::Null;
                                return Ok(());
                            }
                        }

                        if output.is_null() {
                            *output = Value::Object(Object::default());
                        }
                        let output_object = output.as_object_mut().ok_or(InvalidValue)?;

                        match self.apply_selection_set(
                            selection_set,
                            parameters,
                            input_object,
                            output_object,
                            path,
                            &FieldType::Named(type_name.to_string()),
                        ) {
                            Ok(()) => Ok(()),
                            Err(InvalidValue) => {
                                *output = Value::Null;
                                Ok(())
                            }
                        }
                    }
                    _ => {
                        *output = Value::Null;
                        Ok(())
                    }
                }
            }

            // the rest of the possible types just need to validate the expected value
            FieldType::Int => {
                let opt = if input.is_i64() {
                    input.as_i64().and_then(|i| i32::try_from(i).ok())
                } else if input.is_u64() {
                    input.as_i64().and_then(|i| i32::try_from(i).ok())
                } else {
                    None
                };

                // if the value is invalid, we do not insert it in the output object
                // which is equivalent to inserting null
                if opt.is_some() {
                    *output = input.clone();
                } else {
                    *output = Value::Null;
                }
                Ok(())
            }
            FieldType::Float => {
                if input.as_f64().is_some() {
                    *output = input.clone();
                } else {
                    *output = Value::Null;
                }
                Ok(())
            }
            FieldType::Boolean => {
                if input.as_bool().is_some() {
                    *output = input.clone();
                } else {
                    *output = Value::Null;
                }
                Ok(())
            }
            FieldType::String => {
                if input.as_str().is_some() {
                    *output = input.clone();
                } else {
                    *output = Value::Null;
                }
                Ok(())
            }
            FieldType::Id => {
                if input.is_string() || input.is_i64() || input.is_u64() || input.is_f64() {
                    *output = input.clone();
                } else {
                    *output = Value::Null;
                }
                Ok(())
            }
        }
    }

    fn apply_selection_set(
        &self,
        selection_set: &[Selection],
        parameters: &mut FormatParameters,
        input: &mut Object,
        output: &mut Object,
        path: &mut Path,
        parent_type: &FieldType,
    ) -> Result<(), InvalidValue> {
        // For skip and include, using .unwrap_or is legit here because
        // validate_variables should have already checked that
        // the variable is present and it is of the correct type
        for selection in selection_set {
            match selection {
                Selection::Field {
                    name,
                    alias,
                    selection_set,
                    field_type,
                    skip,
                    include,
                } => {
                    let field_name = alias.as_ref().unwrap_or(name);
                    if skip.should_skip(parameters.variables).unwrap_or(false) {
                        continue;
                    }

                    if !include.should_include(parameters.variables).unwrap_or(true) {
                        continue;
                    }

                    if let Some(input_value) = input.get_mut(field_name.as_str()) {
                        // if there's already a value for that key in the output it means either:
                        // - the value is a scalar and was moved into output using take(), replacing
                        // the input value with Null
                        // - the value was already null and is already present in output
                        // if we expect an object or list at that key, output will already contain
                        // an object or list and then input_value cannot be null
                        if input_value.is_null() && output.contains_key(field_name.as_str()) {
                            continue;
                        }

                        let selection_set = selection_set.as_deref().unwrap_or_default();
                        let output_value =
                            output.entry((*field_name).clone()).or_insert(Value::Null);
                        if field_name.as_str() == TYPENAME {
                            if input_value.is_string() {
                                *output_value = input_value.clone();
                            }
                        } else {
                            path.push(PathElement::Key(field_name.as_str().to_string()));
                            let res = self.format_value(
                                parameters,
                                field_type,
                                input_value,
                                output_value,
                                path,
                                parent_type,
                                selection_set,
                            );
                            path.pop();
                            res?
                        }
                    } else {
                        if !output.contains_key(field_name.as_str()) {
                            output.insert((*field_name).clone(), Value::Null);
                        }
                        if field_type.is_non_null() {
                            parameters.errors.push(Error {
                                message: format!(
                                    "Cannot return null for non-nullable field {parent_type}.{}",
                                    field_name.as_str()
                                ),
                                path: Some(path.clone()),
                                ..Error::default()
                            });

                            return Err(InvalidValue);
                        }
                    }
                }
                Selection::InlineFragment {
                    type_condition,
                    selection_set,
                    skip,
                    include,
                    known_type,
                } => {
                    if skip.should_skip(parameters.variables).unwrap_or(false) {
                        continue;
                    }

                    if !include.should_include(parameters.variables).unwrap_or(true) {
                        continue;
                    }

                    let is_apply = if let Some(input_type) =
                        input.get(TYPENAME).and_then(|val| val.as_str())
                    {
                        // check if the fragment matches the input type directly, and if not, check if the
                        // input type is a subtype of the fragment's type condition (interface, union)
                        input_type == type_condition.as_str()
                            || parameters.schema.is_subtype(type_condition, input_type)
                    } else {
                        // known_type = true means that from the query's shape, we know
                        // we should get the right type here. But in the case we get a
                        // __typename field and it does not match, we should not apply
                        // that fragment
                        // If the type condition is an interface and the current known type implements it
                        known_type
                            .as_ref()
                            .map(|k| parameters.schema.is_subtype(type_condition, k))
                            .unwrap_or_default()
                            || known_type.as_deref() == Some(type_condition.as_str())
                    };

                    if is_apply {
                        self.apply_selection_set(
                            selection_set,
                            parameters,
                            input,
                            output,
                            path,
                            parent_type,
                        )?;
                    }
                }
                Selection::FragmentSpread {
                    name,
                    known_type,
                    skip,
                    include,
                } => {
                    if skip.should_skip(parameters.variables).unwrap_or(false) {
                        continue;
                    }

                    if !include.should_include(parameters.variables).unwrap_or(true) {
                        continue;
                    }

                    if let Some(fragment) = self.fragments.get(name) {
                        if fragment
                            .skip
                            .should_skip(parameters.variables)
                            .unwrap_or(false)
                        {
                            continue;
                        }
                        if !fragment
                            .include
                            .should_include(parameters.variables)
                            .unwrap_or(true)
                        {
                            continue;
                        }

                        let is_apply = if let Some(input_type) =
                            input.get(TYPENAME).and_then(|val| val.as_str())
                        {
                            // check if the fragment matches the input type directly, and if not, check if the
                            // input type is a subtype of the fragment's type condition (interface, union)
                            input_type == fragment.type_condition.as_str()
                                || parameters
                                    .schema
                                    .is_subtype(&fragment.type_condition, input_type)
                        } else {
                            // If the type condition is an interface and the current known type implements it
                            known_type
                                .as_ref()
                                .map(|k| parameters.schema.is_subtype(&fragment.type_condition, k))
                                .unwrap_or_default()
                                || known_type.as_deref() == Some(fragment.type_condition.as_str())
                        };

                        if is_apply {
                            self.apply_selection_set(
                                &fragment.selection_set,
                                parameters,
                                input,
                                output,
                                path,
                                parent_type,
                            )?;
                        }
                    } else {
                        // the fragment should have been already checked with the schema
                        failfast_debug!("missing fragment named: {}", name);
                    }
                }
            }
        }

        Ok(())
    }

    fn apply_root_selection_set(
        &self,
        operation: &Operation,
        parameters: &mut FormatParameters,
        input: &mut Object,
        output: &mut Object,
        path: &mut Path,
    ) -> Result<(), InvalidValue> {
        for selection in &operation.selection_set {
            match selection {
                Selection::Field {
                    name,
                    alias,
                    selection_set,
                    field_type,
                    skip,
                    include,
                } => {
                    // Using .unwrap_or is legit here because
                    // validate_variables should have already checked that
                    // the variable is present and it is of the correct type
                    if skip.should_skip(parameters.variables).unwrap_or(false) {
                        continue;
                    }

                    if !include.should_include(parameters.variables).unwrap_or(true) {
                        continue;
                    }

                    let field_name = alias.as_ref().unwrap_or(name);
                    let field_name_str = field_name.as_str();
                    if let Some(input_value) = input.get_mut(field_name_str) {
                        // if there's already a value for that key in the output it means either:
                        // - the value is a scalar and was moved into output using take(), replacing
                        // the input value with Null
                        // - the value was already null and is already present in output
                        // if we expect an object or list at that key, output will already contain
                        // an object or list and then input_value cannot be null
                        if input_value.is_null() && output.contains_key(field_name_str) {
                            continue;
                        }

                        let selection_set = selection_set.as_deref().unwrap_or_default();
                        let output_value =
                            output.entry((*field_name).clone()).or_insert(Value::Null);
                        path.push(PathElement::Key(field_name_str.to_string()));
                        let res = self.format_value(
                            parameters,
                            field_type,
                            input_value,
                            output_value,
                            path,
                            field_type,
                            selection_set,
                        );
                        path.pop();
                        res?
                    } else if field_name_str == TYPENAME {
                        if !output.contains_key(field_name_str) {
                            output.insert(
                                field_name.clone(),
                                Value::String(operation.kind.to_string().into()),
                            );
                        }
                    } else if field_type.is_non_null() {
                        parameters.errors.push(Error {
                            message: format!(
                                "Cannot return null for non-nullable field {}.{field_name_str}",
                                operation.kind
                            ),
                            path: Some(path.clone()),
                            ..Error::default()
                        });
                        return Err(InvalidValue);
                    } else {
                        output.insert(field_name.clone(), Value::Null);
                    }
                }
                Selection::InlineFragment {
                    type_condition,
                    selection_set,
                    ..
                } => {
                    // top level objects will not provide a __typename field
                    if type_condition.as_str()
                        != parameters.schema.root_operation_name(operation.kind)
                    {
                        return Err(InvalidValue);
                    }

                    self.apply_selection_set(
                        selection_set,
                        parameters,
                        input,
                        output,
                        path,
                        &FieldType::Named(type_condition.clone()),
                    )?;
                }
                Selection::FragmentSpread {
                    name,
                    known_type: _,
                    skip: _,
                    include: _,
                } => {
                    if let Some(fragment) = self.fragments.get(name) {
                        let operation_type_name =
                            parameters.schema.root_operation_name(operation.kind);
                        let is_apply = {
                            // check if the fragment matches the input type directly, and if not, check if the
                            // input type is a subtype of the fragment's type condition (interface, union)
                            operation_type_name == fragment.type_condition.as_str()
                                || parameters
                                    .schema
                                    .is_subtype(&fragment.type_condition, operation_type_name)
                        };

                        if !is_apply {
                            return Err(InvalidValue);
                        }

                        self.apply_selection_set(
                            &fragment.selection_set,
                            parameters,
                            input,
                            output,
                            path,
                            &FieldType::Named(operation_type_name.into()),
                        )?;
                    } else {
                        // the fragment should have been already checked with the schema
                        failfast_debug!("missing fragment named: {}", name);
                    }
                }
            }
        }

        Ok(())
    }

    /// Validate a [`Request`]'s variables against this [`Query`] using a provided [`Schema`].
    #[tracing::instrument(skip_all, level = "trace")]
    pub(crate) fn validate_variables(
        &self,
        request: &Request,
        schema: &Schema,
    ) -> Result<(), Response> {
        let operation_name = request.operation_name.as_deref();
        let operation_variable_types =
            self.operations
                .iter()
                .fold(HashMap::new(), |mut acc, operation| {
                    if operation_name.is_none() || operation.name.as_deref() == operation_name {
                        acc.extend(operation.variables.iter().map(|(k, v)| (k.as_str(), v)))
                    }
                    acc
                });

        if LevelFilter::current() >= LevelFilter::DEBUG {
            let known_variables = operation_variable_types.keys().cloned().collect();
            let provided_variables = request
                .variables
                .keys()
                .map(|k| k.as_str())
                .collect::<HashSet<_>>();
            let unknown_variables = provided_variables
                .difference(&known_variables)
                .collect::<Vec<_>>();
            if !unknown_variables.is_empty() {
                failfast_debug!(
                    "Received variable unknown to the query: {:?}",
                    unknown_variables,
                );
            }
        }

        let errors = operation_variable_types
            .iter()
            .filter_map(|(name, (ty, _))| {
                let value = request.variables.get(*name).unwrap_or(&Value::Null);
                ty.validate_input_value(value, schema).err().map(|_| {
                    FetchError::ValidationInvalidTypeVariable {
                        name: name.to_string(),
                    }
                    .to_graphql_error(None)
                })
            })
            .collect::<Vec<_>>();

        if errors.is_empty() {
            Ok(())
        } else {
            Err(Response::builder().errors(errors).build())
        }
    }

    pub(crate) fn contains_only_typename(&self) -> bool {
        self.operations.len() == 1 && self.operations[0].is_only_typename()
    }

    pub(crate) fn contains_introspection(&self) -> bool {
        self.operations.iter().any(Operation::is_introspection)
    }
}

/// Intermediate structure for arguments passed through the entire formatting
struct FormatParameters<'a> {
    variables: &'a Object,
    errors: Vec<Error>,
    schema: &'a Schema,
}

#[derive(Debug)]
pub(crate) struct Operation {
    name: Option<String>,
    kind: OperationKind,
    selection_set: Vec<Selection>,
    variables: HashMap<ByteString, (FieldType, Option<Value>)>,
}

impl Operation {
    // clippy false positive due to the bytes crate
    // ref: https://rust-lang.github.io/rust-clippy/master/index.html#mutable_key_type
    #[allow(clippy::mutable_key_type)]
    // Spec: https://spec.graphql.org/draft/#sec-Language.Operations
    fn from_ast(operation: ast::OperationDefinition, schema: &Schema) -> Result<Self, SpecError> {
        let name = operation.name().map(|x| x.text().to_string());

        let kind = operation
            .operation_type()
            .and_then(|op| {
                op.query_token()
                    .map(|_| OperationKind::Query)
                    .or_else(|| op.mutation_token().map(|_| OperationKind::Mutation))
                    .or_else(|| op.subscription_token().map(|_| OperationKind::Subscription))
            })
            .unwrap_or(OperationKind::Query);

        let current_field_type = match kind {
            OperationKind::Query => FieldType::Named("Query".to_string()),
            OperationKind::Mutation => FieldType::Named("Mutation".to_string()),
            OperationKind::Subscription => return Err(SpecError::SubscriptionNotSupported),
        };

        let selection_set = operation
            .selection_set()
            .expect("the node SelectionSet is not optional in the spec; qed")
            .selections()
            .map(|selection| Selection::from_ast(selection, &current_field_type, schema, 0))
            .collect::<Result<Vec<Option<_>>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<Selection>>();

        let variables = operation
            .variable_definitions()
            .iter()
            .flat_map(|x| x.variable_definitions())
            .map(|definition| {
                let name = definition
                    .variable()
                    .expect("the node Variable is not optional in the spec; qed")
                    .name()
                    .expect("the node Name is not optional in the spec; qed")
                    .text()
                    .to_string();
                let ty = FieldType::from(
                    definition
                        .ty()
                        .expect("the node Type is not optional in the spec; qed"),
                );

                (
                    ByteString::from(name),
                    (ty, parse_default_value(&definition)),
                )
            })
            .collect();

        Ok(Operation {
            selection_set,
            name,
            variables,
            kind,
        })
    }

    /// A query or mutation containing only `__typename` at the root level
    fn is_only_typename(&self) -> bool {
        self.selection_set.len() == 1
            && self
                .selection_set
                .get(0)
                .map(|s| matches!(s, Selection::Field {name, ..} if name.as_str() == TYPENAME))
                .unwrap_or_default()
    }

    fn is_introspection(&self) -> bool {
        // If the only field is `__typename` it's considered as an introspection query
        if self.is_only_typename() {
            return true;
        }
        self.selection_set.iter().all(|sel| match sel {
            Selection::Field { name, .. } => {
                let name = name.as_str();
                // `__typename` can only be resolved in runtime,
                // so this query cannot be seen as an introspection query
                name == "__schema" || name == "__type"
            }
            _ => false,
        })
    }

    pub(crate) fn kind(&self) -> &OperationKind {
        &self.kind
    }
}

impl From<ast::OperationType> for OperationKind {
    // Spec: https://spec.graphql.org/draft/#OperationType
    fn from(operation_type: ast::OperationType) -> Self {
        if operation_type.query_token().is_some() {
            Self::Query
        } else if operation_type.mutation_token().is_some() {
            Self::Mutation
        } else if operation_type.subscription_token().is_some() {
            Self::Subscription
        } else {
            unreachable!(
                "either the `query` token is provided, either the `mutation` token, \
                either the `subscription` token; qed"
            )
        }
    }
}

fn parse_default_value(definition: &ast::VariableDefinition) -> Option<Value> {
    definition
        .default_value()
        .and_then(|v| v.value())
        .and_then(|value| parse_value(&value))
}

fn parse_value(value: &ast::Value) -> Option<Value> {
    match value {
        ast::Value::Variable(_) => None,
        ast::Value::StringValue(s) => Some(s.to_string().into()),
        ast::Value::FloatValue(f) => f.to_string().parse::<f64>().ok().map(Into::into),
        ast::Value::IntValue(i) => {
            let s = i.to_string();
            s.parse::<i64>()
                .ok()
                .map(Into::into)
                .or_else(|| s.parse::<u64>().ok().map(Into::into))
        }
        ast::Value::BooleanValue(b) => {
            match (b.true_token().is_some(), b.false_token().is_some()) {
                (true, false) => Some(Value::Bool(true)),
                (false, true) => Some(Value::Bool(false)),
                _ => None,
            }
        }
        ast::Value::NullValue(_) => Some(Value::Null),
        ast::Value::EnumValue(e) => e.name().map(|n| n.text().to_string().into()),
        ast::Value::ListValue(l) => l
            .values()
            .map(|v| parse_value(&v))
            .collect::<Option<_>>()
            .map(Value::Array),
        ast::Value::ObjectValue(o) => o
            .object_fields()
            .map(|field| match (field.name(), field.value()) {
                (Some(name), Some(value)) => {
                    parse_value(&value).map(|v| (name.text().to_string().into(), v))
                }
                _ => None,
            })
            .collect::<Option<_>>()
            .map(Value::Object),
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;
    use test_log::test;

    use super::*;
    use crate::json_ext::ValueExt;

    macro_rules! assert_eq_and_ordered {
        ($a:expr, $b:expr $(,)?) => {
            assert_eq!($a, $b,);
            assert!(
                $a.eq_and_ordered(&$b),
                "assertion failed: objects are not ordered the same:\
                \n  left: `{:?}`\n right: `{:?}`",
                $a,
                $b,
            );
        };
    }

    #[derive(Default)]
    struct FormatTest {
        schema: Option<&'static str>,
        query: Option<&'static str>,
        operation: Option<&'static str>,
        variables: Option<serde_json_bytes::Value>,
        response: Option<serde_json_bytes::Value>,
        expected: Option<serde_json_bytes::Value>,
        expected_errors: Option<serde_json_bytes::Value>,
        federation_version: FederationVersion,
    }

    enum FederationVersion {
        Fed1,
        Fed2,
    }

    impl Default for FederationVersion {
        fn default() -> Self {
            FederationVersion::Fed1
        }
    }

    impl FormatTest {
        fn builder() -> Self {
            Self::default()
        }

        fn schema(mut self, schema: &'static str) -> Self {
            self.schema = Some(schema);
            self
        }

        fn query(mut self, query: &'static str) -> Self {
            self.query = Some(query);
            self
        }

        fn operation(mut self, operation: &'static str) -> Self {
            self.operation = Some(operation);
            self
        }

        fn response(mut self, v: serde_json_bytes::Value) -> Self {
            self.response = Some(v);
            self
        }

        fn variables(mut self, v: serde_json_bytes::Value) -> Self {
            self.variables = Some(v);
            self
        }

        fn expected(mut self, v: serde_json_bytes::Value) -> Self {
            self.expected = Some(v);
            self
        }

        fn expected_errors(mut self, v: serde_json_bytes::Value) -> Self {
            self.expected_errors = Some(v);
            self
        }

        fn fed2(mut self) -> Self {
            self.federation_version = FederationVersion::Fed2;
            self
        }

        #[track_caller]
        fn test(self) {
            let schema = self.schema.expect("missing schema");
            let query = self.query.expect("missing query");
            let response = self.response.expect("missing response");

            let schema = match self.federation_version {
                FederationVersion::Fed1 => with_supergraph_boilerplate(schema),
                FederationVersion::Fed2 => with_supergraph_boilerplate_fed2(schema),
            };

            let schema =
                Schema::parse(&schema, &Default::default()).expect("could not parse schema");

            let api_schema = schema.api_schema();
            let query =
                Query::parse(query, &schema, &Default::default()).expect("could not parse query");
            let mut response = Response::builder().data(response.clone()).build();

            query.format_response(
                &mut response,
                self.operation,
                self.variables
                    .unwrap_or_else(|| Value::Object(Object::default()))
                    .as_object()
                    .unwrap()
                    .clone(),
                api_schema,
            );

            if let Some(e) = self.expected {
                assert_eq_and_ordered!((&response).data.as_ref().unwrap(), &e);
            }

            if let Some(e) = self.expected_errors {
                assert_eq_and_ordered!(serde_json_bytes::to_value(&response.errors).unwrap(), e);
            }
        }
    }

    fn with_supergraph_boilerplate(content: &str) -> String {
        format!(
            "{}\n{}",
            r#"
        schema
            @core(feature: "https://specs.apollo.dev/core/v0.1")
            @core(feature: "https://specs.apollo.dev/join/v0.1")
            @core(feature: "https://specs.apollo.dev/inaccessible/v0.1")
             {
            query: Query
        }
        directive @core(feature: String!) repeatable on SCHEMA
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        directive @inaccessible on OBJECT | FIELD_DEFINITION | INTERFACE | UNION
        enum join__Graph {
            TEST @join__graph(name: "test", url: "http://localhost:4001/graphql")
        }

        "#,
            content
        )
    }

    fn with_supergraph_boilerplate_fed2(content: &str) -> String {
        format!(
            "{}\n{}",
            r#"
            schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
            @link(url: "https://specs.apollo.dev/inaccessible/v0.2", for: SECURITY)
            {
                query: Query
            }

            directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
            directive @join__graph(name: String!, url: String!) on ENUM_VALUE
            directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
            directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
            directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
            directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ENUM | ENUM_VALUE | SCALAR | INPUT_OBJECT | INPUT_FIELD_DEFINITION | ARGUMENT_DEFINITION

            scalar join__FieldSet
            scalar link__Import
            enum link__Purpose {
            """
            `SECURITY` features provide metadata necessary to securely resolve fields.
            """
            SECURITY

            """
            `EXECUTION` features provide metadata necessary for operation execution.
            """
            EXECUTION
            }

            enum join__Graph {
                TEST @join__graph(name: "test", url: "http://localhost:4001/graphql")
            }
        "#,
            content
        )
    }

    #[test]
    fn reformat_response_data_field() {
        FormatTest::builder()
            .schema(
                "type Query {
            foo: String
            stuff: Bar
            array: [Bar]
            baz: String
        }
        type Bar {
            bar: String
            baz: String
        }",
            )
            .query(
                "query Test {
            foo
            stuff{bar __typename }
            array{bar}
            baz
            alias:baz
            alias_obj:stuff{bar}
            alias_array:array{bar}
        }",
            )
            .response(json! {{
                "foo": "1",
                "stuff": {"bar": "2", "__typename": "Bar"},
                "array": [{"bar": "3", "baz": "4"}, {"bar": "5", "baz": "6"}],
                "baz": "7",
                "alias": "7",
                "alias_obj": {"bar": "8"},
                "alias_array": [{"bar": "9", "baz": "10"}, {"bar": "11", "baz": "12"}],
                "other": "13",
            }})
            .expected(json! {{
                "foo": "1",
                "stuff": {
                    "bar": "2",
                    "__typename": "Bar",
                },
                "array": [
                    {"bar": "3"},
                    {"bar": "5"},
                ],
                "baz": "7",
                "alias": "7",
                "alias_obj": {
                    "bar": "8",
                },
                "alias_array": [
                    {"bar": "9"},
                    {"bar": "11"},
                ],
            }})
            .test();
    }

    #[test]
    fn reformat_response_data_inline_fragment() {
        let schema = "type Query {
            get: Test
            getStuff: Stuff
          }
  
          type Stuff {
              stuff: Bar
          }
          type Bar {
              bar: String
          }
          type Thing {
              id: String
          }
          union Test = Stuff | Thing";
        let query = "{ get { ... on Stuff { stuff{bar}} ... on Thing { id }} }";

        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {
                {"get": {"__typename": "Stuff", "id": "1", "stuff": {"bar": "2"}}}
            })
            .expected(json! {{
                "get": {
                    "stuff": {
                        "bar": "2",
                    },
                }
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {
                {"get": {"__typename": "Thing", "id": "1", "stuff": {"bar": "2"}}}
            })
            .expected(json! {{
                "get": {
                    "id": "1",
                }
            }})
            .test();
    }

    #[test]
    fn inline_fragment_on_top_level_operation() {
        let schema = "type Query {
            get: Test
            getStuff: Stuff
          }

          type Stuff {
              stuff: Bar
          }
          type Bar {
              bar: String
          }
          type Thing {
              id: String
          }
          union Test = Stuff | Thing";

        // when using a fragment on an operation exported by a subgraph,
        // we might not get a __typename field, we should instead be able
        // to know the type in advance
        FormatTest::builder()
            .schema(schema)
            .query("{ getStuff { ... on Stuff { stuff{bar}} ... on Thing { id }} }")
            .response(json! {
                {"getStuff": { "stuff": {"bar": "2"}}}
            })
            .expected(json! {{
                 "getStuff": {
                    "stuff": {"bar": "2"},
                }
            }})
            .test();
    }

    #[test]
    fn reformat_response_data_fragment_spread() {
        let schema = "type Query {
          thing: Thing    
        }

        type Foo {
            foo: String
        }
        type Bar {
            bar: String
        }
        type Baz {
            baz: String
        }
        union Thing = Foo
        extend union Thing = Bar | Baz";
        let query = "query { thing {...foo ...bar ...baz} } fragment foo on Foo {foo} fragment bar on Bar {bar} fragment baz on Baz {baz}";

        // should only select fields from Foo
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {
                {"thing": {"__typename": "Foo", "foo": "1", "bar": "2", "baz": "3"}}
            })
            .expected(json! {
                {"thing": {"foo": "1"}}
            })
            .test();

        // should only select fields from Bar
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {
                {"thing": {"__typename": "Bar", "foo": "1", "bar": "2", "baz": "3"}}
            })
            .expected(json! {
                {"thing": {"bar": "2"} }
            })
            .test();

        // should only select fields from Baz
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {
                {"thing": {"__typename": "Baz", "foo": "1", "bar": "2", "baz": "3"}}
            })
            .expected(json! {
                {"thing": {"baz": "3"} }
            })
            .test();
    }

    #[test]
    fn reformat_response_data_best_effort() {
        FormatTest::builder()
            .schema(
                "type Query {
            get: Thing
        }
        type Thing {
            foo: String
            stuff: Baz
            array: [Element]
            other: Bar
        }

        type Baz {
            bar: String
            baz: String
        }

        type Bar {
            bar: String
        }

        union Element = Baz | Bar
        ",
            )
            .query("{get {foo stuff{bar baz} array{... on Baz { bar baz } } other{bar}}}")
            .response(json! {
                {
                    "get": {
                        "foo": "1",
                        "stuff": {"baz": "2"},
                        "array": [
                            {"baz": "3"},
                            "4",
                            {"bar": "5"},
                        ],
                        "other": "6",
                    },
                    "should_be_removed": {
                        "aaa": 2
                    },
                }
            })
            .expected(json! {
                {
                    "get": {
                        "foo": "1",
                        "stuff": {
                            "bar": null,
                            "baz": "2",
                        },
                        "array": [
                            {},
                            null,
                            {},
                        ],
                        "other": null,
                    },
                }
            })
            .test();
    }

    #[test]
    fn reformat_response_array_of_scalar_simple() {
        FormatTest::builder()
            .schema(
                "type Query {
                    get: Thing
                }
                type Thing {
                    array: [Int]
                }",
            )
            .query("{get {array}}")
            .response(json! {{
                "get": {
                    "array": [1,2,3,4],
                },
            }})
            .expected(json! {{
                "get": {
                    "array": [1,2,3,4],
                },
            }})
            .test();
    }

    #[test]
    fn reformat_response_array_of_scalar_alias() {
        FormatTest::builder()
            .schema(
                "type Query {
                    get: Thing
                }
                type Thing {
                    array: [Int]
                }",
            )
            .query("{get {stuff: array}}")
            .response(json! {{
                "get": {
                    "stuff": [1,2,3,4],
                },
            }})
            .expected(json! {{
                "get": {
                    "stuff": [1,2,3,4],
                },
            }})
            .test();
    }

    #[test]
    fn reformat_response_array_of_scalar_duplicate_alias() {
        FormatTest::builder()
            .schema(
                "type Query {
                get: Thing
            }
            type Thing {
                array: [Int]
            }",
            )
            .query("{get {array stuff:array}}")
            .response(json! {{
                "get": {
                    "array": [1,2,3,4],
                    "stuff": [1,2,3,4],
                },
            }})
            .expected(json! {{
                "get": {
                    "array": [1,2,3,4],
                    "stuff": [1,2,3,4],
                },
            }})
            .test();
    }

    #[test]
    fn reformat_response_array_of_scalar_duplicate_key() {
        FormatTest::builder()
            .schema(
                "type Query {
                    get: Thing
                }
                type Thing {
                    array: [Int]
                }",
            )
            .query("{get {array array}}")
            .response(json! {{
                "get": {
                    "array": [1,2,3,4],
                },
            }})
            .expected(json! {{
                "get": {
                    "array": [1,2,3,4],
                },
            }})
            .test();
    }

    #[test]
    fn reformat_response_array_of_type_simple() {
        FormatTest::builder()
            .schema(
                "type Query {
                get: Thing
            }
            type Thing {
                array: [Element]
            }

            type Element {
                stuff: String
            }
            ",
            )
            .query("{get {array{stuff}}}")
            .response(json! {{
                "get": {
                    "array": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                },
            }})
            .expected(json! {{
                "get": {
                    "array": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                },
            }})
            .test();
    }

    #[test]
    fn reformat_response_array_of_type_alias() {
        FormatTest::builder()
            .schema(
                "type Query {
                    get: Thing
                }
                type Thing {
                    array: [Element]
                }
    
                type Element {
                    stuff: String
                }
                
            ",
            )
            .query("{get { aliased: array {stuff}}}")
            .response(json! {{
                "get": {
                    "aliased": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                },
            }})
            .expected(json! {{
                "get": {
                    "aliased": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                },
            }})
            .test();
    }

    #[test]
    fn reformat_response_array_of_type_duplicate() {
        FormatTest::builder()
            .schema(
                "type Query {
                    get: Thing
                }
                type Thing {
                    array: [Element]
                }
    
                type Element {
                    stuff: String
                }
                ",
            )
            .query("{get {array{stuff} array{stuff}}}")
            .response(json! {{
                "get": {
                    "array": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                },
            }})
            .expected(json! {{
                "get": {
                    "array": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                },
            }})
            .test();
    }

    #[test]
    fn reformat_response_array_of_type_duplicate_alias() {
        FormatTest::builder()
            .schema(
                "type Query {
                get: Thing
            }
            type Thing {
                array: [Element]
            }
            
            type Element {
                stuff: String
            }",
            )
            .query("{get {array{stuff} aliased: array{stuff}}}")
            .response(json! {{
                "get": {
                    "array": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                    "aliased": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                },
            }})
            .expected(json! {{
                "get": {
                    "array": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                    "aliased": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                },
            }})
            .test();
    }

    #[test]
    fn reformat_response_array_of_enum_simple() {
        FormatTest::builder()
            .schema(
                "type Query {
                    get: Thing
                }
                type Thing {
                    array: [Element]
                }
    
                enum Element {
                    FOO
                    BAR
                }",
            )
            .query("{get {array}}")
            .response(json! {{
                "get": {
                    "array": ["FOO", "BAR"],
                },
            }})
            .expected(json! {{
                "get": {
                    "array": ["FOO", "BAR"],
                },
            }})
            .test();
    }

    #[test]
    fn reformat_response_array_of_enum_alias() {
        FormatTest::builder()
            .schema(
                "type Query {
                get: Thing
            }
            type Thing {
                array: [Element]
            }

            enum Element {
                FOO
                BAR
            }",
            )
            .query("{get {stuff: array}}")
            .response(json! {{
                "get": {
                    "stuff": ["FOO", "BAR"],
                },
            }})
            .expected(json! {{
                "get": {
                    "stuff": ["FOO", "BAR"],
                },
            }})
            .test();
    }

    #[test]
    fn reformat_response_array_of_enum_duplicate() {
        FormatTest::builder()
            .schema(
                "type Query {
                    get: Thing
                }
                type Thing {
                    array: [Element]
                }
    
                enum Element {
                    FOO
                    BAR
                }",
            )
            .query("{get {array array}}")
            .response(json! {{
                "get": {
                    "array": ["FOO", "BAR"],
                },
            }})
            .expected(json! {{
                "get": {
                    "array": ["FOO", "BAR"],
                },
            }})
            .test();
    }

    #[test]
    fn reformat_response_array_of_enum_duplicate_alias() {
        FormatTest::builder()
            .schema(
                "type Query {
                get: Thing
            }
            type Thing {
                array: [Element]
            }

            enum Element {
                FOO
                BAR
            }",
            )
            .query("{get {array stuff: array}}")
            .response(json! {{
                "get": {
                    "array": ["FOO", "BAR"],
                    "stuff": ["FOO", "BAR"],
                },
            }})
            .expected(json! {{
                "get": {
                    "array": ["FOO", "BAR"],
                    "stuff": ["FOO", "BAR"],
                },
            }})
            .test();
    }

    #[test]
    // If this test fails, this means you got greedy about allocations,
    // beware of aliases!
    fn reformat_response_array_of_int_duplicate() {
        FormatTest::builder()
            .schema(
                "type Query {
                    get: Thing
                }
                type Thing {
                    array: [Int]
                }",
            )
            .query("{get {array array}}")
            .response(json! {{
                "get": {
                    "array": [1,2,3,4],
                },
            }})
            .expected(json! {{
                "get": {
                    "array": [1,2,3,4],
                },
            }})
            .test();
    }

    #[test]
    fn reformat_response_array_of_float_duplicate() {
        FormatTest::builder()
            .schema(
                "type Query {
                    get: Thing
                }
                type Thing {
                    array: [Float]
                }",
            )
            .query("{get {array array}}")
            .response(json! {{
                "get": {
                    "array": [1,2,3,4],
                },
            }})
            .expected(json! {{
                "get": {
                    "array": [1,2,3,4],
                },
            }})
            .test();
    }

    #[test]
    fn reformat_response_array_of_bool_duplicate() {
        FormatTest::builder()
            .schema(
                "type Query {
                    get: Thing
                }
                type Thing {
                    array: [Boolean]
                }",
            )
            .query("{get {array array}}")
            .response(json! {{
                "get": {
                    "array": [true,false],
                },
            }})
            .expected(json! {{
                "get": {
                    "array": [true,false],
                },
            }})
            .test();
    }

    #[test]
    fn reformat_response_array_of_string_duplicate() {
        FormatTest::builder()
            .schema(
                "type Query {
                    get: Thing
                }
                type Thing {
                    array: [String]
                }",
            )
            .query("{get {array array}}")
            .response(json! {{
                "get": {
                    "array": ["hello","world"],
                },
            }})
            .expected(json! {{
                "get": {
                    "array": ["hello","world"],
                },
            }})
            .test();
    }

    #[test]
    fn reformat_response_array_of_id_duplicate() {
        FormatTest::builder()
            .schema(
                "type Query {
                    get: Thing
                }
                type Thing {
                    array: [ID]
                }",
            )
            .query("{get {array array}}")
            .response(json! {{
                "get": {
                    "array": ["hello","world"],
                },
            }})
            .expected(json! {{
                "get": {
                    "array": ["hello","world"],
                },
            }})
            .test();
    }

    #[test]
    fn solve_query_with_single_typename() {
        FormatTest::builder()
            .schema(
                "type Query {
                    get: Thing
                }
                type Thing {
                    array: [String]
                }",
            )
            .query("{ __typename }")
            .response(json! {{}})
            .expected(json! {{
                "__typename": "Query"
            }})
            .test();
    }

    #[test]
    fn reformat_response_query_with_root_typename() {
        FormatTest::builder()
            .schema(
                "type Query {
                    get: Thing
                }
                type Thing {
                    foo: String
                }",
            )
            .query("{get {foo __typename} __typename}")
            .response(json! {{
                "get": {
                    "foo": "1",
                    "__typename": "Thing"
                }
            }})
            .expected(json! {{
                "get": {
                    "foo": "1",
                    "__typename": "Thing"
                },
                "__typename": "Query",
            }})
            .test();
    }

    macro_rules! run_validation {
        ($schema:expr, $query:expr, $variables:expr $(,)?) => {{
            let variables = match $variables {
                Value::Object(object) => object,
                _ => unreachable!("variables must be an object"),
            };
            let schema =
                Schema::parse(&$schema, &Default::default()).expect("could not parse schema");
            let request = Request::builder()
                .variables(variables)
                .query($query.to_string())
                .build();
            let query = Query::parse(
                request
                    .query
                    .as_ref()
                    .expect("query has been added right above; qed"),
                &schema,
                &Default::default(),
            )
            .expect("could not parse query");
            query.validate_variables(&request, &schema)
        }};
    }

    macro_rules! assert_validation {
        ($schema:expr, $query:expr, $variables:expr $(,)?) => {{
            let res = run_validation!(with_supergraph_boilerplate($schema), $query, $variables);
            assert!(res.is_ok(), "validation should have succeeded: {:?}", res);
        }};
    }

    macro_rules! assert_validation_error {
        ($schema:expr, $query:expr, $variables:expr $(,)?) => {{
            let res = run_validation!(with_supergraph_boilerplate($schema), $query, $variables);
            assert!(res.is_err(), "validation should have failed");
        }};
    }

    #[test]
    fn variable_validation() {
        let schema = "type Query { x: String }";
        // https://spec.graphql.org/June2018/#sec-Int
        assert_validation!(schema, "query($foo:Int){x}", json!({}));
        assert_validation_error!(schema, "query($foo:Int!){x}", json!({}));
        // When expected as an input type, only integer input values are accepted.
        assert_validation!(schema, "query($foo:Int){x}", json!({"foo":2}));
        assert_validation!(schema, "query($foo:Int){x}", json!({ "foo": i32::MAX }));
        assert_validation!(schema, "query($foo:Int){x}", json!({ "foo": i32::MIN }));
        // All other input values, including strings with numeric content, must raise a query error indicating an incorrect type.
        assert_validation_error!(schema, "query($foo:Int){x}", json!({"foo":"2"}));
        assert_validation_error!(schema, "query($foo:Int){x}", json!({"foo":2.0}));
        assert_validation_error!(schema, "query($foo:Int){x}", json!({"foo":"str"}));
        assert_validation_error!(schema, "query($foo:Int){x}", json!({"foo":true}));
        assert_validation_error!(schema, "query($foo:Int){x}", json!({"foo":{}}));
        //  If the integer input value represents a value less than -231 or greater than or equal to 231, a query error should be raised.
        assert_validation_error!(
            schema,
            "query($foo:Int){x}",
            json!({ "foo": i32::MAX as i64 + 1 })
        );
        assert_validation_error!(
            schema,
            "query($foo:Int){x}",
            json!({ "foo": i32::MIN as i64 - 1 })
        );

        // https://spec.graphql.org/draft/#sec-Float.Input-Coercion
        assert_validation!(schema, "query($foo:Float){x}", json!({}));
        assert_validation_error!(schema, "query($foo:Float!){x}", json!({}));

        // When expected as an input type, both integer and float input values are accepted.
        assert_validation!(schema, "query($foo:Float){x}", json!({"foo":2}));
        assert_validation!(schema, "query($foo:Float){x}", json!({"foo":2.0}));
        // All other input values, including strings with numeric content,
        // must raise a request error indicating an incorrect type.
        assert_validation_error!(schema, "query($foo:Float){x}", json!({"foo":"2.0"}));
        assert_validation_error!(schema, "query($foo:Float){x}", json!({"foo":"2"}));

        // https://spec.graphql.org/June2018/#sec-String
        assert_validation!(schema, "query($foo:String){x}", json!({}));
        assert_validation_error!(schema, "query($foo:String!){x}", json!({}));

        // When expected as an input type, only valid UTF‐8 string input values are accepted.
        assert_validation!(schema, "query($foo:String){x}", json!({"foo": "str"}));

        // All other input values must raise a query error indicating an incorrect type.
        assert_validation_error!(schema, "query($foo:String){x}", json!({"foo":true}));
        assert_validation_error!(schema, "query($foo:String){x}", json!({"foo": 0}));
        assert_validation_error!(schema, "query($foo:String){x}", json!({"foo": 42.0}));
        assert_validation_error!(schema, "query($foo:String){x}", json!({"foo": {}}));

        // https://spec.graphql.org/June2018/#sec-Boolean
        assert_validation!(schema, "query($foo:Boolean){x}", json!({}));
        assert_validation_error!(schema, "query($foo:Boolean!){x}", json!({}));
        // When expected as an input type, only boolean input values are accepted.
        // All other input values must raise a query error indicating an incorrect type.
        assert_validation!(schema, "query($foo:Boolean!){x}", json!({"foo":true}));
        assert_validation_error!(schema, "query($foo:Boolean!){x}", json!({"foo":"true"}));
        assert_validation_error!(schema, "query($foo:Boolean!){x}", json!({"foo": 0}));
        assert_validation_error!(schema, "query($foo:Boolean!){x}", json!({"foo": "no"}));

        // https://spec.graphql.org/June2018/#sec-ID
        assert_validation!(schema, "query($foo:ID){x}", json!({}));
        assert_validation_error!(schema, "query($foo:ID!){x}", json!({}));
        // When expected as an input type, any string (such as "4") or integer (such as 4)
        // input value should be coerced to ID as appropriate for the ID formats a given GraphQL server expects.
        assert_validation!(schema, "query($foo:ID){x}", json!({"foo": 4}));
        assert_validation!(schema, "query($foo:ID){x}", json!({"foo": "4"}));
        assert_validation!(schema, "query($foo:String){x}", json!({"foo": "str"}));
        assert_validation!(schema, "query($foo:String){x}", json!({"foo": "4.0"}));
        // Any other input value, including float input values (such as 4.0), must raise a query error indicating an incorrect type.
        assert_validation_error!(schema, "query($foo:ID){x}", json!({"foo": 4.0}));
        assert_validation_error!(schema, "query($foo:ID){x}", json!({"foo": true}));
        assert_validation_error!(schema, "query($foo:ID){x}", json!({"foo": {}}));

        // https://spec.graphql.org/June2018/#sec-Type-System.List
        assert_validation!(schema, "query($foo:[Int]){x}", json!({}));
        assert_validation!(schema, "query($foo:[Int!]){x}", json!({}));
        assert_validation!(schema, "query($foo:[Int!]){x}", json!({ "foo": null }));
        assert_validation!(schema, "query($foo:[Int]){x}", json!({"foo":1}));
        assert_validation!(schema, "query($foo:[String]){x}", json!({"foo":"bar"}));
        assert_validation!(schema, "query($foo:[[Int]]){x}", json!({"foo":1}));
        assert_validation!(
            schema,
            "query($foo:[[Int]]){x}",
            json!({"foo":[[1], [2, 3]]})
        );
        assert_validation_error!(schema, "query($foo:[Int]){x}", json!({"foo":"str"}));
        assert_validation_error!(schema, "query($foo:[Int]){x}", json!({"foo":{}}));
        assert_validation_error!(schema, "query($foo:[Int]!){x}", json!({}));
        assert_validation_error!(schema, "query($foo:[Int!]){x}", json!({"foo":[1, null]}));
        assert_validation!(schema, "query($foo:[Int]!){x}", json!({"foo":[]}));
        assert_validation!(schema, "query($foo:[Int]){x}", json!({"foo":[1,2,3]}));
        assert_validation_error!(schema, "query($foo:[Int]){x}", json!({"foo":["f","o","o"]}));
        assert_validation_error!(schema, "query($foo:[Int]){x}", json!({"foo":["1","2","3"]}));
        assert_validation!(
            schema,
            "query($foo:[String]){x}",
            json!({"foo":["1","2","3"]})
        );
        assert_validation_error!(schema, "query($foo:[String]){x}", json!({"foo":[1,2,3]}));
        assert_validation!(schema, "query($foo:[Int!]){x}", json!({"foo":[1,2,3]}));
        assert_validation_error!(schema, "query($foo:[Int!]){x}", json!({"foo":[1,null,3]}));
        assert_validation!(schema, "query($foo:[Int]){x}", json!({"foo":[1,null,3]}));

        // https://spec.graphql.org/June2018/#sec-Input-Objects
        assert_validation!(
            "input Foo{ y: String } type Query { x: String }",
            "query($foo:Foo){x}",
            json!({})
        );
        assert_validation!(
            "input Foo{ y: String } type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":{}})
        );
        assert_validation_error!(
            "input Foo{ y: String } type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":1})
        );
        assert_validation_error!(
            "input Foo{ y: String } type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":"str"})
        );
        assert_validation_error!(
            "input Foo{x:Int!} type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":{}})
        );
        assert_validation!(
            "input Foo{x:Int!} type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":{"x":1}})
        );
        assert_validation!(
            "scalar Foo type Query { x: String }",
            "query($foo:Foo!){x}",
            json!({"foo":{}})
        );
        assert_validation!(
            "scalar Foo type Query { x: String }",
            "query($foo:Foo!){x}",
            json!({"foo":1})
        );
        assert_validation_error!(
            "scalar Foo type Query { x: String }",
            "query($foo:Foo!){x}",
            json!({})
        );
        assert_validation!(
            "input Foo{bar:Bar!} input Bar{x:Int!} type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":{"bar":{"x":1}}})
        );
        assert_validation!(
            "enum Availability{AVAILABLE} type Product{availability:Availability! name:String} type Query{products(availability: Availability!): [Product]!}",
            "query GetProductsByAvailability($availability: Availability!){products(availability: $availability) {name}}",
            json!({"availability": "AVAILABLE"})
        );

        assert_validation!(
            "input MessageInput {
                content: String
                author: String
              }
              type Receipt {
                  id: ID!
              }
              type Query{
                  send(message: MessageInput): String}",
            "query {
                send(message: {
                    content: \"Hello\"
                    author: \"Me\"
                }) {
                    id
                }}",
            json!({"availability": "AVAILABLE"})
        );

        assert_validation!(
            "input MessageInput {
                content: String
                author: String
              }
              type Receipt {
                  id: ID!
              }
              type Query{
                  send(message: MessageInput): String}",
            "query($msg: MessageInput) {
                send(message: $msg) {
                    id
                }}",
            json!({"msg":  {
                "content": "Hello",
                "author": "Me"
            }})
        );
    }

    #[test]
    fn filter_root_errors() {
        let schema = "type Query {
            getInt: Int
            getNonNullString: String!
        }";

        FormatTest::builder()
            .schema(schema)
            .query("query MyOperation { getInt }")
            .response(json! {{
                "getInt": "not_an_int",
                "other": "2",
            }})
            .operation("MyOperation")
            .expected(json! {{
                "getInt": null,
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query("query { getNonNullString }")
            .response(json! {{
                "getNonNullString": 1,
            }})
            .expected(Value::Null)
            .test();
    }

    #[test]
    fn filter_object_errors() {
        let schema = "type Query {
            me: User
        }

        type User {
            id: String!
            name: String
        }";
        let query = "query  { me { id name } }";

        // name expected a string, got an int
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "name": null,
                },
            }})
            .test();

        // non null id expected a string, got an int
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "me": {
                    "id": 1,
                    "name": 1,
                },
            }})
            .expected(json! {{
                "me": null,
            }})
            .test();

        // non null id got a null
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "me": {
                    "id": null,
                },
            }})
            .expected(json! {{
                "me": null,
            }})
            .test();

        // non null id was absent
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "me": { },
            }})
            .expected(json! {{
                "me": null,
            }})
            .test();

        // non null id was absent
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "me": {
                    "name": 1,
                },
            }})
            .expected(json! {{
                "me": null,
            }})
            .test();

        // a non null field not present in the query should not be an error
        FormatTest::builder()
            .schema(schema)
            .query("query  { me { name } }")
            .response(json! {{
                "me": {
                    "name": "a",
                },
            }})
            .expected(json! {{
                "me": {
                    "name": "a",
                },
            }})
            .test();

        // if a field appears multiple times, selection should be deduplicated
        FormatTest::builder()
            .schema(schema)
            .query("query  { me { id id } }")
            .response(json! {{
                "me": {
                    "id": "a",
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                },
            }})
            .test();

        // duplicate id field
        FormatTest::builder()
            .schema(schema)
            .query("query  { me { id ...on User { id } } }")
            .response(json! {{
                "me": {
                    "__typename": "User",
                    "id": "a",
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                },
            }})
            .test();
    }

    #[test]
    fn filter_list_errors() {
        let schema = "type Query {
            list: TestList
        }

        type TestList {
            l1: [String]
            l2: [String!]
            l3: [String]!
            l4: [String!]!
        }";

        // l1: nullable list of nullable elements
        // any error should stop at the list elements
        FormatTest::builder()
            .schema(schema)
            .query("query { list { l1 } }")
            .response(json! {{
                "list": {
                    "l1": ["abc", 1, { "foo": "bar"}, ["aaa"], "def"],
                    "name": 1,
                },
            }})
            .expected(json! {{
                "list": {
                    "l1": ["abc", null, null, null, "def"],
                },
            }})
            .test();

        // l1 expected a list, got a string
        FormatTest::builder()
            .schema(schema)
            .query("query { list { l1 } }")
            .response(json! {{
                "list": {
                    "l1": "abc",
                },
            }})
            .expected(json! {{
                "list": {
                    "l1": null,
                },
            }})
            .test();

        // l2: nullable list of non nullable elements
        // any element error should nullify the entire list
        FormatTest::builder()
            .schema(schema)
            .query("query { list { l2 } }")
            .response(json! {{
                "list": {
                    "l2": ["abc", 1, { "foo": "bar"}, ["aaa"], "def"],
                    "name": 1,
                },
            }})
            .expected(json! {{
                "list": {
                    "l2": null,
                },
            }})
            .expected_errors(json! {[
                {
                    "message": "Cannot return null for non-nullable array element of type String at index 1",
                    "path": ["list", "l2", 1]
                }
            ]},)
            .test();

        FormatTest::builder()
            .schema(schema)
            .query("query { list { l2 } }")
            .response(json! {{
                "list": {
                    "l2": ["abc", "def"],
                    "name": 1,
                },
            }})
            .expected(json! {{
                "list": {
                    "l2": ["abc", "def"],
                },
            }})
            .test();

        // l3: nullable list of nullable elements
        // any element error should stop at the list elements
        FormatTest::builder()
            .schema(schema)
            .query("query { list { l3 } }")
            .response(json! {{
                "list": {
                    "l3": ["abc", 1, { "foo": "bar"}, ["aaa"], "def"],
                    "name": 1,
                },
            }})
            .expected(json! {{
                "list": {
                    "l3": ["abc", null, null, null, "def"],
                },
            }})
            .test();

        // non null l3 expected a list, got an int, parrent element should be null
        FormatTest::builder()
            .schema(schema)
            .query("query { list { l3 } }")
            .response(json! {{
                "list": {
                    "l3": 1,
                },
            }})
            .expected(json! {{
                "list": null,
            }})
            .test();

        // l4: non nullable list of non nullable elements
        // any element error should nullify the entire list,
        // which will nullify the parent element
        FormatTest::builder()
            .schema(schema)
            .query("query { list { l4 } }")
            .response(json! {{
                "list": {
                    "l4": ["abc", 1, { "foo": "bar"}, ["aaa"], "def"],
                },
            }})
            .expected(json! {{
                "list": null,
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query("query { list { l4 } }")
            .response(json! {{
                "list": {
                    "l4": ["abc", "def"],
                },
            }})
            .expected(json! {{
                "list": {
                    "l4": ["abc", "def"],
                },
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query("query { list { l4 } }")
            .response(json! {{
                "list": {
                    "l4": 1,
                },
            }})
            .expected(json! {{
                "list": null,
            }})
            .test();
    }

    #[test]
    fn filter_nested_object_errors() {
        let schema = "type Query {
            me: User
        }

        type User {
            id: String!
            name: String
            reviews1: [Review]
            reviews2: [Review!]
            reviews3: [Review!]!
        }
        
        type Review {
            text1: String
            text2: String!
        }
        ";

        // nullable parent and child elements
        // child errors should stop at the child's level
        let query_review1_text1 = "query  { me { id reviews1 { text1 } } }";
        FormatTest::builder()
            .schema(schema)
            .query(query_review1_text1)
            .response(json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews1": [ { } ],
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "reviews1": [ {
                        "text1": null,
                    } ],
                },
            }})
            .test();

        // nullable text1 was null
        FormatTest::builder()
            .schema(schema)
            .query(query_review1_text1)
            .response(json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews1": [ { "text1": null } ],
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "reviews1": [ { "text1": null } ],
                },
            }})
            .test();

        // nullable text1 expected a string, got an int, so text1 is nullified
        FormatTest::builder()
            .schema(schema)
            .query(query_review1_text1)
            .response(json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews1": [ { "text1": 1 } ],
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "reviews1": [ { "text1": null } ],
                },
            }})
            .test();

        // text2 is non null so errors should nullify reviews1 element
        let query_review1_text2 = "query  { me { id reviews1 { text2 } } }";
        // text2 was absent, reviews1 element should be nullified
        FormatTest::builder()
            .schema(schema)
            .query(query_review1_text2)
            .response(json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews1": [ { } ],
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "reviews1": [ null ],
                },
            }})
            .expected_errors(json! {[
                {
                    "message": "Cannot return null for non-nullable field Named type Review.text2",
                    "path": ["me", "reviews1", 0]
                }
            ]})
            .test();

        // text2 was null, reviews1 element should be nullified
        FormatTest::builder()
            .schema(schema)
            .query(query_review1_text2)
            .response(json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews1": [ { "text2": null } ],
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "reviews1": [ null ],
                },
            }})
            .test();

        // text2 expected a string, got an int, text2 is nullified, reviews1 element should be nullified
        FormatTest::builder()
            .schema(schema)
            .query(query_review1_text2)
            .response(json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews1": [ { "text2": 1 } ],
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "reviews1": [ null ],
                },
            }})
            .test();

        // reviews2: [Review!]
        // reviews2 elements are non null, so any error there should nullify the entire list
        let query_review2_text1 = "query  { me { id reviews2 { text1 } } }";
        // nullable text1 was absent
        FormatTest::builder()
            .schema(schema)
            .query(query_review2_text1)
            .response(json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews2": [ { } ],
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "reviews2": [ {
                        "text1": null,
                    } ],
                },
            }})
            .test();

        // nullable text1 was null
        FormatTest::builder()
            .schema(schema)
            .query(query_review2_text1)
            .response(json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews2": [ { "text1": null } ],
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "reviews2": [ { "text1": null } ],
                },
            }})
            .test();

        // nullable text1 expected a string, got an int
        FormatTest::builder()
            .schema(schema)
            .query(query_review2_text1)
            .response(json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews2": [ { "text1": 1 } ],
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "reviews2": [ { "text1": null } ],
                },
            }})
            .test();

        // text2 is non null
        let query_review2_text2 = "query  { me { id reviews2 { text2 } } }";
        // text2 was absent, so the reviews2 element is nullified, so reviews2 is nullified
        FormatTest::builder()
            .schema(schema)
            .query(query_review2_text2)
            .response(json! {{
                "me": {
                    "id": "a",
                        "name": 1,
                        "reviews2": [ { } ],
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "reviews2": null,
                },
            }})
            .test();

        // text2 was null, so the reviews2 element is nullified, so reviews2 is nullified
        FormatTest::builder()
            .schema(schema)
            .query(query_review2_text2)
            .response(json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews2": [ { "text2": null } ],
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "reviews2": null,
                },
            }})
            .test();

        // text2 expected a string, got an int, so the reviews2 element is nullified, so reviews2 is nullified
        FormatTest::builder()
            .schema(schema)
            .query(query_review2_text2)
            .response(json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews2": [ { "text2": 1 } ],
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "reviews2": null,
                },
            }})
            .test();

        //reviews3: [Review!]!
        // reviews3 is non null, and its elements are non null
        let query_review3_text1 = "query  { me { id reviews3 { text1 } } }";

        // nullable text1 was absent
        FormatTest::builder()
            .schema(schema)
            .query(query_review3_text1)
            .response(json! {{
                "me": {
                    "id": "a",
                        "name": 1,
                        "reviews3": [ { } ],
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "reviews3": [ {
                        "text1": null,
                    } ],
                },
            }})
            .test();

        // nullable text1 was null
        FormatTest::builder()
            .schema(schema)
            .query(query_review3_text1)
            .response(json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews3": [ { "text1": null } ],
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "reviews3": [ { "text1": null } ],
                },
            }})
            .test();

        // nullable text1 expected a string, got an int
        FormatTest::builder()
            .schema(schema)
            .query(query_review3_text1)
            .response(json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews3": [ { "text1": 1 } ],
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "reviews3": [ { "text1": null } ],
                },
            }})
            .test();

        // reviews3 is non null, and its elements are non null, text2 is non null
        let query_review3_text2 = "query  { me { id reviews3 { text2 } } }";

        // text2 was absent, nulls should propagate up to the operation
        FormatTest::builder()
            .schema(schema)
            .query(query_review3_text2)
            .response(json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews3": [ { } ],
                },
            }})
            .expected(json! {{
                "me": null,

            }})
            .test();

        // text2 was null, nulls should propagate up to the operation
        FormatTest::builder()
            .schema(schema)
            .query(query_review3_text2)
            .response(json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews3": [ { "text2": null } ],
                },
            }})
            .expected(json! {{
                "me": null,

            }})
            .test();

        // text2 expected a string, got an int, nulls should propagate up to the operation
        FormatTest::builder()
            .schema(schema)
            .query(query_review3_text2)
            .response(json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews3": [ { "text2": 1 } ],
                },
            }})
            .expected(json! {{
                "me": null,

            }})
            .test();
    }

    #[test]
    fn filter_alias_errors() {
        let schema = "type Query {
            me: User
        }

        type User {
            id: String!
            name: String
        }";
        let query = "query  { me { id identifiant:id } }";

        // both aliases got valid values
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "me": {
                    "id": "a",
                    "identifiant": "b",
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "identifiant": "b",
                },
            }})
            .test();

        // non null identifiant expected a string, got an int, the operation should be null
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "me": {
                    "id": "a",
                    "identifiant": 1,
                },
            }})
            .expected(json! {{
               "me": null,
            }})
            .test();

        // non null identifiant was null, the operation should be null
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "me": {
                    "id": "a",
                    "identifiant": null,
                },
            }})
            .expected(json! {{
               "me": null,
            }})
            .test();

        // non null identifiant was absent, the operation should be null
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "me": {
                    "id": "a",
                },
            }})
            .expected(json! {{
               "me": null,
            }})
            .test();

        let query2 = "query  { me { name name2:name } }";

        // both aliases got valid values
        FormatTest::builder()
            .schema(schema)
            .query(query2)
            .response(json! {{
                "me": {
                    "name": "a",
                    "name2": "b",
                },
            }})
            .expected(json! {{
                "me": {
                    "name": "a",
                    "name2": "b",
                },
            }})
            .test();

        // nullable name2 expected a string, got an int, name2 should be null
        FormatTest::builder()
            .schema(schema)
            .query(query2)
            .response(json! {{
                "me": {
                    "name": "a",
                    "name2": 1,
                },
            }})
            .expected(json! {{
                "me": {
                    "name": "a",
                    "name2": null,
                },
            }})
            .test();

        // nullable name2 was null
        FormatTest::builder()
            .schema(schema)
            .query(query2)
            .response(json! {{
                "me": {
                    "name": "a",
                    "name2": null,
                },
            }})
            .expected(json! {{
                "me": {
                    "name": "a",
                    "name2": null,
                },
            }})
            .test();

        // nullable name2 was absent
        FormatTest::builder()
            .schema(schema)
            .query(query2)
            .response(json! {{
                "me": {
                    "name": "a",
                },
            }})
            .expected(json! {{
                "me": {
                    "name": "a",
                    "name2": null,
                },
            }})
            .test();
    }

    #[test]
    fn filter_scalar_errors() {
        let schema = "type Query {
            me: User
        }

        type User {
            id: String!
            a: A
            b: A!
        }
        
        scalar A
        ";

        let query = "query  { me { id a } }";

        // scalar a is present, no further validation
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "me": {
                    "id": "a",
                    "a": "hello",
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "a": "hello",
                },
            }})
            .test();

        // scalar a is present, no further validation=
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "me": {
                    "id": "a",
                    "a": {
                        "field": 1234,
                    },
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "a": {
                        "field": 1234,
                    },
                },
            }})
            .test();

        let query2 = "query  { me { id b } }";

        // non null scalar b is present, no further validation
        FormatTest::builder()
            .schema(schema)
            .query(query2)
            .response(json! {{
                "me": {
                    "id": "a",
                    "b": "hello",
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "b": "hello",
                },
            }})
            .test();

        // non null scalar b is present, no further validation
        FormatTest::builder()
            .schema(schema)
            .query(query2)
            .response(json! {{
                "me": {
                    "id": "a",
                    "b": {
                        "field": 1234,
                    },
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "b": {
                        "field": 1234,
                    },
                },
            }})
            .test();

        // non null scalar b was null, the operation should be null
        FormatTest::builder()
            .schema(schema)
            .query(query2)
            .response(json! {{
                "me": {
                    "id": "a",
                    "b": null,
                },
            }})
            .expected(json! {{
                "me": null,
            }})
            .test();

        // non null scalar b was absent, the operation should be null
        FormatTest::builder()
            .schema(schema)
            .query(query2)
            .response(json! {{
                "me": {
                    "id": "a",
                },
            }})
            .expected(json! {{
                "me": null,
            }})
            .test();
    }

    #[test]
    fn filter_enum_errors() {
        let schema = "type Query {
            me: User
        }

        type User {
            id: String!
            a: A
            b: A!
        }

        enum A {
            X
            Y
            Z
        }";

        let query_a = "query  { me { id a } }";

        // enum a got a correct value
        FormatTest::builder()
            .schema(schema)
            .query(query_a)
            .response(json! {{
                "me": {
                    "id": "a",
                    "a": "X",
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "a": "X",
                },
            }})
            .test();

        // nullable enum a expected "X", "Y" or "Z", got another string, a should be null
        FormatTest::builder()
            .schema(schema)
            .query(query_a)
            .response(json! {{
                "me": {
                    "id": "a",
                    "a": "hello",
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "a": null,
                },
            }})
            .test();

        // nullable enum a was null
        FormatTest::builder()
            .schema(schema)
            .query(query_a)
            .response(json! {{
                "me": {
                    "id": "a",
                    "a": null,
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "a": null,
                },
            }})
            .test();

        let query_b = "query  { me { id b } }";

        // non nullable enum b got a correct value
        FormatTest::builder()
            .schema(schema)
            .query(query_b)
            .response(json! {{
                "me": {
                    "id": "a",
                    "b": "X",
                },
            }})
            .expected(json! {{
                "me": {
                    "id": "a",
                    "b": "X",
                },
            }})
            .test();

        // non nullable enum b expected "X", "Y" or "Z", got another string, b and the operation should be null
        FormatTest::builder()
            .schema(schema)
            .query(query_b)
            .response(json! {{
                "me": {
                    "id": "a",
                    "b": "hello",
                },
            }})
            .expected(json! {{
                "me": null,
            }})
            .test();

        // non nullable enum b was null, the operation should be null
        FormatTest::builder()
            .schema(schema)
            .query(query_b)
            .response(json! {{
                "me": {
                    "id": "a",
                    "b": null,
                },
            }})
            .expected(json! {{
                "me": null,
            }})
            .test();
    }

    #[test]
    fn filter_interface_errors() {
        let schema = "type Query {
            me: NamedEntity
        }

        interface NamedEntity {
            name: String
            name2: String!
        }

        type User implements NamedEntity {
            name: String
            name2: String!
        }

        type User2 implements NamedEntity {
            name: String
            name2: String!
        }
        ";

        let query = "query  { me { name } }";

        // nullable name field got a correct value
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "me": {
                    "name": "a",
                },
            }})
            .expected(json! {{
                "me": {
                    "name": "a",
                },
            }})
            .test();

        // nullable name field was absent
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "me": { },
            }})
            .expected(json! {{
                "me": {
                    "name": null,
                },
            }})
            .test();

        // nullable name field was null
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "me": {
                    "name": null,
                },
            }})
            .expected(json! {{
                "me": {
                    "name": null,
                },
            }})
            .test();

        // nullable name field expected a string, got an int
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "me": {
                    "name": 1,
                },
            }})
            .expected(json! {{
                "me": {
                    "name": null,
                },
            }})
            .test();

        let query2 = "query  { me { name2 } }";

        // non nullable name2 field got a correct value
        FormatTest::builder()
            .schema(schema)
            .query(query2)
            .response(json! {{
                "me": {
                    "name2": "a",
                },
            }})
            .expected(json! {{
                "me": {
                    "name2": "a",
                },
            }})
            .test();

        // non nullable name2 field was absent, the operation should be null
        FormatTest::builder()
            .schema(schema)
            .query(query2)
            .response(json! {{
                "me": { },
            }})
            .expected(json! {{
                "me": null,
            }})
            .test();

        // non nullable name2 field was null, the operation should be null
        FormatTest::builder()
            .schema(schema)
            .query(query2)
            .response(json! {{
                "me": {
                    "name2": null,
                },
            }})
            .expected(json! {{
                "me": null,
            }})
            .test();

        // non nullable name2 field expected a string, got an int, name2 and the operation should be null
        FormatTest::builder()
            .schema(schema)
            .query(query2)
            .response(json! {{
                "me": {
                    "name2": 1,
                },
            }})
            .expected(json! {{
                "me": null,
            }})
            .test();

        // we should be able to handle duplicate fields even across fragments and interfaces
        FormatTest::builder()
            .schema(schema)
            .query("query { me { ... on User { name2 } name2 } }")
            .response(json! {{
                "me": {
                    "__typename": "User",
                    "name2": "a",
                },
            }})
            .expected(json! {{
                "me": {
                    "name2": "a",
                },
            }})
            .test();
    }

    #[test]
    fn filter_extended_interface_errors() {
        let schema = "type Query {
            me: NamedEntity
        }

        interface NamedEntity {
            name: String
        }

        type User implements NamedEntity {
            name: String
        }

        type User2 implements NamedEntity {
            name: String
        }

        extend interface NamedEntity {
            name2: String!
        }

        extend type User {
            name2: String!
        }

        extend type User2 {
            name2: String!
        }
        ";

        let query = "query  { me { name2 } }";

        // non nullable name2 got a correct value
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "me": {
                    "name2": "a",
                },
            }})
            .expected(json! {{
                "me": {
                    "name2": "a",
                },
            }})
            .test();

        // non nullable name2 was null, the operation should be null
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "me": {
                    "name2": null,
                },
            }})
            .expected(json! {{
                "me": null,
            }})
            .test();

        // non nullable name2 was absent, the operation should be null
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "me": { },
            }})
            .expected(json! {{
                "me": null,
            }})
            .test();

        // non nullable name2 expected a string, got an int, the operation should be null
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "me": {
                    "name2": 1,
                },
            }})
            .expected(json! {{
                "me": null,
            }})
            .test();
    }

    #[test]
    fn filter_errors_top_level_fragment() {
        let schema = "type Query {
            get: Thing   
          }
  
          type Thing {
              name: String
              name2: String!
          }";

        // fragments can appear on top level queries
        let query = "{ ...frag } fragment frag on Query { __typename get { name } }";

        // nullable name got a correct value
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "get": {
                    "name": "a",
                    "other": "b"
                }
            }})
            .expected(json! {{
                "__typename": null,
                "get": {
                    "name": "a",
                }
            }})
            .test();

        // nullable name was null
        FormatTest::builder()
            .schema(schema)
            .query(query)
            .response(json! {{
                "get": {"name": null, "other": "b"}
            }})
            .expected(json! {{
                "__typename": null,
                "get": {
                    "name": null,
                }
            }})
            .test();

        let query2 = "{ ...frag2 } fragment frag2 on Query { __typename get { name2 } }";
        // non nullable name2 got a correct value
        FormatTest::builder()
            .schema(schema)
            .query(query2)
            .response(json! {{
                "get": {"name2": "a", "other": "b"}
            }})
            .expected(json! {{
                "__typename": null,
                "get": {
                    "name2": "a",
                }
            }})
            .test();

        // non nullable name2 was null, the operation should be null
        FormatTest::builder()
            .schema(schema)
            .query(query2)
            .response(json! {{
                "get": {"name2": null, "other": "b"}
            }})
            .expected(json! {{
                "__typename": null,
                "get": null
            }})
            .test();

        let query3 = "{ ... on Query { __typename get { name } } }";
        // nullable name got a correct value
        FormatTest::builder()
            .schema(schema)
            .query(query3)
            .response(json! {{
                "get": {"name": "a", "other": "b"}
            }})
            .expected(json! {{
                "__typename": null,
                "get": {
                    "name": "a",
                }
            }})
            .test();

        // nullable name was null
        FormatTest::builder()
            .schema(schema)
            .query(query3)
            .response(json! {{
                "get": {"name": null, "other": "b"}
            }})
            .expected(json! {{
                "__typename": null,
                "get": {
                    "name": null,
                }
            }})
            .test();

        let query4 = "{ ... on Query { __typename get { name2 } } }";
        // non nullable name2 got a correct value
        FormatTest::builder()
            .schema(schema)
            .query(query4)
            .response(json! {{
                "get": {"name2": "a", "other": "b"}
            }})
            .expected(json! {{
                "__typename": null,
                "get": {
                    "name2": "a",
                }
            }})
            .test();

        // non nullable name2 was null, the operation should be null
        FormatTest::builder()
            .schema(schema)
            .query(query4)
            .response(json! {{
                "get": {"name2": null, "other": "b"}
            }})
            .expected(json! {{
                "__typename": null,
                "get": null,
            }})
            .test();
    }

    #[test]
    fn merge_selections() {
        let schema = "type Query {
            get: Product
        }

        type Product {
            id: String!
            name: String
            review: Review
        }
        
        type Review {
            id: String!
            body: String
        }";

        // duplicate operation name
        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                get {
                    id
                }
                get {
                    name
                }
            }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .test();

        // merge nested selection
        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                get {
                    id
                    review {
                        id
                    }

                    ... on Product {
                        review {
                            body
                        }
                    }
                }
                get {
                    name
                }
            }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "review": {
                        "__typename": "Review",
                        "id": "b",
                        "body": "hello",
                    }
                },
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                    "review": {
                        "id": "b",
                        "body": "hello",
                    },
                    "name": null,
                },
            }})
            .test();
    }

    #[test]
    fn it_statically_includes() {
        let schema = with_supergraph_boilerplate(
            "type Query {
            name: String
            review: Review
            product: Product
        }

        type Product {
            id: String!
            name: String
            review: Review
        }

        type Review {
            id: String!
            body: String
        }",
        );
        let schema = Schema::parse(&schema, &Default::default()).expect("could not parse schema");

        let query = Query::parse(
            "query  {
                name @include(if: false)
                review @include(if: false)
                product @include(if: true) {
                    name
                }
            }",
            &schema,
            &Default::default(),
        )
        .expect("could not parse query");
        assert_eq!(query.operations.len(), 1);
        let operation = &query.operations[0];
        assert_eq!(operation.selection_set.len(), 1);
        match operation.selection_set.get(0).unwrap() {
            Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("product")),
            _ => panic!("expected a field"),
        }

        let query = Query::parse(
            "query  {
                name @include(if: false)
                review
                product @include(if: true) {
                    name
                }
            }",
            &schema,
            &Default::default(),
        )
        .expect("could not parse query");

        assert_eq!(query.operations.len(), 1);
        let operation = &query.operations[0];
        assert_eq!(operation.selection_set.len(), 2);
        match operation.selection_set.get(0).unwrap() {
            Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("review")),
            _ => panic!("expected a field"),
        }
        match operation.selection_set.get(1).unwrap() {
            Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("product")),
            _ => panic!("expected a field"),
        }

        // Inline fragment
        let query = Query::parse(
            "query  {
                name @include(if: false)
                ... @include(if: false) {
                    review
                }
                product @include(if: true) {
                    name
                }
            }",
            &schema,
            &Default::default(),
        )
        .expect("could not parse query");

        assert_eq!(query.operations.len(), 1);
        let operation = &query.operations[0];
        assert_eq!(operation.selection_set.len(), 1);
        match operation.selection_set.get(0).unwrap() {
            Selection::Field {
                name,
                selection_set: Some(selection_set),
                ..
            } => {
                assert_eq!(name, &ByteString::from("product"));
                assert_eq!(selection_set.len(), 1);
            }
            _ => panic!("expected a field"),
        }

        // Fragment spread
        let query = Query::parse(
            "
            fragment ProductName on Product {
                name
            }
            query  {
                name @include(if: false)
                review
                product @include(if: true) {
                    ...ProductName @include(if: false)
                }
            }",
            &schema,
            &Default::default(),
        )
        .expect("could not parse query");

        assert_eq!(query.operations.len(), 1);
        let operation = &query.operations[0];
        assert_eq!(operation.selection_set.len(), 2);
        match operation.selection_set.get(0).unwrap() {
            Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("review")),
            _ => panic!("expected a field"),
        }
        match operation.selection_set.get(1).unwrap() {
            Selection::Field {
                name,
                selection_set: Some(selection_set),
                ..
            } => {
                assert_eq!(name, &ByteString::from("product"));
                assert_eq!(selection_set.len(), 0);
            }
            _ => panic!("expected a field"),
        }
    }

    #[test]
    fn it_statically_skips() {
        let schema = with_supergraph_boilerplate(
            "type Query {
            name: String
            review: Review
            product: Product
        }

        type Product {
            id: String!
            name: String
            review: Review
        }

        type Review {
            id: String!
            body: String
        }",
        );
        let schema = Schema::parse(&schema, &Default::default()).expect("could not parse schema");

        let query = Query::parse(
            "query  {
                name @skip(if: true)
                review @skip(if: true)
                product @skip(if: false) {
                    name
                }
            }",
            &schema,
            &Default::default(),
        )
        .expect("could not parse query");
        assert_eq!(query.operations.len(), 1);
        let operation = &query.operations[0];
        assert_eq!(operation.selection_set.len(), 1);
        match operation.selection_set.get(0).unwrap() {
            Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("product")),
            _ => panic!("expected a field"),
        }

        let query = Query::parse(
            "query  {
                name @skip(if: true)
                review
                product @skip(if: false) {
                    name
                }
            }",
            &schema,
            &Default::default(),
        )
        .expect("could not parse query");

        assert_eq!(query.operations.len(), 1);
        let operation = &query.operations[0];
        assert_eq!(operation.selection_set.len(), 2);
        match operation.selection_set.get(0).unwrap() {
            Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("review")),
            _ => panic!("expected a field"),
        }
        match operation.selection_set.get(1).unwrap() {
            Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("product")),
            _ => panic!("expected a field"),
        }

        // Inline fragment
        let query = Query::parse(
            "query  {
                name @skip(if: true)
                ... @skip(if: true) {
                    review
                }
                product @skip(if: false) {
                    name
                }
            }",
            &schema,
            &Default::default(),
        )
        .expect("could not parse query");

        assert_eq!(query.operations.len(), 1);
        let operation = &query.operations[0];
        assert_eq!(operation.selection_set.len(), 1);
        match operation.selection_set.get(0).unwrap() {
            Selection::Field {
                name,
                selection_set: Some(selection_set),
                ..
            } => {
                assert_eq!(name, &ByteString::from("product"));
                assert_eq!(selection_set.len(), 1);
            }
            _ => panic!("expected a field"),
        }

        // Fragment spread
        let query = Query::parse(
            "
            fragment ProductName on Product {
                name
            }
            query  {
                name @skip(if: true)
                review
                product @skip(if: false) {
                    ...ProductName @skip(if: true)
                }
            }",
            &schema,
            &Default::default(),
        )
        .expect("could not parse query");

        assert_eq!(query.operations.len(), 1);
        let operation = &query.operations[0];
        assert_eq!(operation.selection_set.len(), 2);
        match operation.selection_set.get(0).unwrap() {
            Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("review")),
            _ => panic!("expected a field"),
        }
        match operation.selection_set.get(1).unwrap() {
            Selection::Field {
                name,
                selection_set: Some(selection_set),
                ..
            } => {
                assert_eq!(name, &ByteString::from("product"));
                assert_eq!(selection_set.len(), 0);
            }
            _ => panic!("expected a field"),
        }
    }

    #[test]
    fn it_should_fail_with_empty_selection_set() {
        let schema = with_supergraph_boilerplate(
            "type Query {
            product: Product
        }

        type Product {
            id: String!
            name: String
        }",
        );
        let schema = Schema::parse(&schema, &Default::default()).expect("could not parse schema");

        let _query_error = Query::parse(
            "query  {
                product {
                }
            }",
            &schema,
            &Default::default(),
        )
        .expect_err("should not parse query");
    }

    #[test]
    fn skip() {
        let schema = "type Query {
            get: Product
        }

        type Product {
            id: String!
            name: String
            review: Review
        }

        type Review {
            id: String!
            body: String
        }";

        // duplicate operation name
        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                    get {
                        name @skip(if: true)
                    }
                    get @skip(if: false) {
                        id 
                        review {
                            id
                        }
                    }
                }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                    "review": {
                        "id": "b",
                    }
                },
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                    "review": {
                        "id": "b",
                    }
                },
            }})
            .test();

        // skipped non null
        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                get {
                    id @skip(if: true)
                    name
                }
            }",
            )
            .response(json! {{
                "get": {
                    "name": "Chair",
                },
            }})
            .expected(json! {{
                "get": {
                    "name": "Chair",
                },
            }})
            .test();

        // inline fragment
        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                get {
                    id
                    ... on Product @skip(if: true) {
                        name
                    }
                }
            }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                },
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
            get {
                id
                ... on Product @skip(if: false) {
                    name
                }
            }
        }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .test();

        // directive on fragment spread
        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                get {
                    id
                    ...test @skip(if: false)
                }
            }

            fragment test on Product {
                nom: name
                name @skip(if: true)
            }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                    "name": "Chair",
                },
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                },
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                    get {
                        id
                        ...test @skip(if: true)
                    }
                }
    
                fragment test on Product {
                    nom: name
                    name @skip(if: true)
                }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                    "name": "Chair",
                },
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                },
            }})
            .test();

        // directive on fragment
        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                get {
                    id
                    ...test
                }
            }

            fragment test on Product @skip(if: false) {
                nom: name
                name @skip(if: true)
            }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                    "name": "Chair",
                },
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                },
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                    get {
                        id
                        ...test
                    }
                }
    
                fragment test on Product @skip(if: true) {
                    nom: name
                    name @skip(if: true)
                }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                    "name": "Chair",
                },
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                },
            }})
            .test();

        // variables
        // duplicate operation name
        FormatTest::builder()
            .schema(schema)
            .query(
                "query Example($shouldSkip: Boolean) {
                    get {
                        id
                        name @skip(if: $shouldSkip)
                    }
                }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .operation("Example")
            .variables(json! {{
                "shouldSkip": true
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                },
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query(
                "query Example($shouldSkip: Boolean) {
                    get {
                        id
                        name @skip(if: $shouldSkip)
                    }
                }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .operation("Example")
            .variables(json! {{
                "shouldSkip": false
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .test();

        // default variable value
        FormatTest::builder()
            .schema(schema)
            .query(
                "query Example($shouldSkip: Boolean) {
                    get {
                        id
                        name @skip(if: $shouldSkip)
                    }
                }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .operation("Example")
            .variables(json! {{
                "shouldSkip": false
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query(
                "query Example($shouldSkip: Boolean = true) {
                    get {
                        id
                        name @skip(if: $shouldSkip)
                    }
                }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .operation("Example")
            .variables(json! {{
                "shouldSkip": false
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query(
                "query Example($shouldSkip: Boolean = true) {
                    get {
                        id
                        name @skip(if: $shouldSkip)
                    }
                }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .operation("Example")
            .expected(json! {{
                "get": {
                    "id": "a",
                },
            }})
            .test();
    }

    #[test]
    fn check_fragment_on_interface() {
        let schema = "type Query {
            get: Product
        }

        interface Product {
            id: String!
            name: String
        }

        type Vodka {
            id: String!
            name: String
        }

        type Beer implements Product {
            id: String!
            name: String
        }";

        FormatTest::builder()
            .schema(schema)
            .query(
                "fragment ProductBase on Product {
                __typename
                id
                name
              }
              query  {
                  get {
                    ...ProductBase
                  }
              }",
            )
            .response(json! {{
                "get": {
                    "__typename": "Beer",
                    "id": "a",
                    "name": "Asahi",
                },
            }})
            .expected(json! {{
                "get": {
                    "__typename": "Beer",
                    "id": "a",
                    "name": "Asahi",
                },
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query(
                "fragment ProductBase on Product {
                id
                name
              }
              query  {
                  get {
                    ...ProductBase
                  }
              }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                        "name": "Asahi",
                },
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                    "name": "Asahi",
                },
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                    get {
                      ... on Product {
                        __typename
                        id
                        name
                      }
                    }
                }",
            )
            .response(json! {{
                "get": {
                    "__typename": "Beer",
                    "id": "a",
                    "name": "Asahi",
                },
            }})
            .expected(json! {{
                "get": {
                    "__typename": "Beer",
                    "id": "a",
                    "name": "Asahi",
                },
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                get {
                  ... on Product {
                    id
                    name
                  }
                }
            }}",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "name": "Asahi",
                },
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                    "name": "Asahi",
                },
            }})
            .test();

        // Make sure we do not return data for type that doesn't implement interface
        FormatTest::builder()
            .schema(schema)
            .query(
                "fragment ProductBase on Product {
                __typename
                id
                name
              }
              query  {
                  get {
                    ...ProductBase
                  }
              }",
            )
            .response(json! {{
                "get": {
                    "__typename": "Vodka",
                    "id": "a",
                    "name": "Crystal",
                },
            }})
            .expected(json! {{
                "get": { }
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                    get {
                      ... on Product {
                        __typename
                        id
                        name
                      }
                    }
                }",
            )
            .response(json! {{
                "get": {
                    "__typename": "Vodka",
                    "id": "a",
                    "name": "Crystal",
                },
            }})
            .expected(json! {{
                "get": { }
            }})
            .test();
    }

    #[test]
    fn include() {
        let schema = "type Query {
            get: Product
        }

        type Product {
            id: String!
            name: String
            review: Review
        }

        type Review {
            id: String!
            body: String
        }";

        // duplicate operation name
        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                get {
                    name @include(if: false)
                }
                get @include(if: true) {
                    id
                    review {
                        id
                    }
                }
            }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                    "review": {
                        "id": "b",
                    }
                },
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                    "review": {
                        "id": "b",
                    }
                },
            }})
            .test();

        // skipped non null
        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                get {
                    id @include(if: false)
                    name
                }
            }",
            )
            .response(json! {{
                "get": {
                    "name": "Chair",
                },
            }})
            .expected(json! {{
                "get": {
                    "name": "Chair",
                },
            }})
            .test();

        // inline fragment
        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                get {
                    id
                    ... on Product @include(if: false) {
                        name
                    }
                }
            }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                },
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                get {
                    id
                    ... on Product @include(if: true) {
                        name
                    }
                }
            }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .test();

        // directive on fragment spread
        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                get {
                    id
                    ...test @include(if: true)
                }
            }

            fragment test on Product {
                nom: name
                name @skip(if: true)
            }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                    "name": "Chair",
                },
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                },
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                    get {
                        id
                        ...test @include(if: false)
                    }
                }
    
                fragment test on Product {
                    nom: name
                    name @include(if: false)
                }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                    "name": "Chair",
                },
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                },
            }})
            .test();

        // directive on fragment
        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                    get {
                        id
                        ...test
                    }
                }
    
                fragment test on Product @include(if: true) {
                    nom: name
                    name @include(if: false)
                }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                    "name": "Chair",
                },
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                },
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                get {
                    id
                    ...test
                }
            }

            fragment test on Product @include(if: false) {
                nom: name
                name @include(if: false)
            }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                    "name": "Chair",
                },
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                },
            }})
            .test();

        // variables
        // duplicate operation name
        FormatTest::builder()
            .schema(schema)
            .query(
                "query Example($shouldInclude: Boolean) {
                get {
                    id
                    name @include(if: $shouldInclude)
                }
            }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .operation("Example")
            .variables(json! {{
                "shouldInclude": false
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                },
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query(
                "query Example($shouldInclude: Boolean) {
                get {
                    id
                    name @include(if: $shouldInclude)
                }
            }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .operation("Example")
            .variables(json! {{
                "shouldInclude": true
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .test();

        // default variable value
        FormatTest::builder()
            .schema(schema)
            .query(
                "query Example($shouldInclude: Boolean = false) {
                get {
                    id
                    name @include(if: $shouldInclude)
                }
            }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .operation("Example")
            .variables(json! {{ }})
            .expected(json! {{
                "get": {
                    "id": "a",
                },
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query(
                "query Example($shouldInclude: Boolean = false) {
                get {
                    id
                    name @include(if: $shouldInclude)
                }
            }",
            )
            .response(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .operation("Example")
            .variables(json! {{
                "shouldInclude": true
            }})
            .expected(json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }})
            .test();
    }

    #[test]
    fn skip_and_include() {
        let schema = "type Query {
            get: Product
        }

        type Product {
            id: String!
            name: String
        }";

        // combine skip and include
        // both of them must accept the field
        // ref: https://spec.graphql.org/October2021/#note-f3059
        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                get {
                    a:name @skip(if:true) @include(if: true)
                    b:name @skip(if:true) @include(if: false)
                    c:name @skip(if:false) @include(if: true)
                    d:name @skip(if:false) @include(if: false)
                }
            }",
            )
            .response(json! {{
                "get": {
                    "a": "a",
                    "b": "b",
                    "c": "c",
                    "d": "d",
                },
            }})
            .expected(json! {{
                "get": {
                    "c": "c",
                },
            }})
            .test();
    }

    #[test]
    fn skip_and_include_multi_operation() {
        let schema = "type Query {
            get: Product
        }

        type Product {
            id: String!
            name: String
            bar: String
        }";

        // combine skip and include
        // both of them must accept the field
        // ref: https://spec.graphql.org/October2021/#note-f3059
        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                get {
                    a:name @skip(if:false)
                }
                get {
                    a:name @skip(if:true)
                }
            }",
            )
            .response(json! {{
                "get": {
                    "a": "a",
                },
            }})
            .expected(json! {{
                "get": {
                    "a": "a",
                },
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                get {
                    a:name @skip(if:true)
                }
                get {
                    a:name @skip(if:false)
                }
            }",
            )
            .response(json! {{
                "get": {
                    "a": "a",
                },
            }})
            .expected(json! {{
                "get": {
                    "a": "a",
                },
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                    get @skip(if: false) @include(if: false) {
                        a:name
                        bar
                    }
                    get @skip(if: false) {
                        a:name
                        a:name
                    }
                }",
            )
            .response(json! {{
                "get": {
                    "a": "a",
                    "bar": "foo",
                },
            }})
            .expected(json! {{
                "get": {
                    "a": "a",
                },
            }})
            .test();
    }

    #[test]
    fn union_with_typename() {
        let schema = "type Query {
            get: ProductResult
        }

        type Product{
            symbol: String!
        }
        type ProductError{
            reason: String
        }
        union ProductResult = Product | ProductError
        ";

        FormatTest::builder()
            .schema(schema)
            .query(
                "query  {
                get {
                    __typename
                    ... on Product {
                      symbol
                    }
                    ... on ProductError {
                      reason
                    }
                }
            }",
            )
            .response(json! {{
                "get": {
                    "__typename": "Product",
                        "symbol": "1"
                },
            }})
            .expected(json! {{
                "get": {
                    "__typename": "Product",
                    "symbol": "1"
                },
            }})
            .test();
    }

    #[test]
    fn inaccessible_on_interface() {
        let schema = "type Query
        {
            test_interface: Interface
            test_union: U
            test_enum: E
        }
        
        type Object implements Interface @inaccessible {
            foo: String
            other: String
        }

        type Object2 implements Interface {
            foo: String
            other: String @inaccessible
        }
          
        interface Interface {
            foo: String
        }

        type A @inaccessible {
            common: String
            a: String
        }

        type B {
            common: String
            b: String
        }
        
        union U = A | B

        enum E {
            X @inaccessible
            Y @inaccessible
            Z
        }
        ";

        FormatTest::builder()
            .schema(schema)
            .fed2()
            .query(
                "query  {
                    test_interface {
                        __typename
                        foo
                    }
    
                    test_interface2: test_interface {
                        __typename
                        foo
                    }
    
                    test_union {
                        ...on B {
                            __typename
                            common
                        }
                    }
    
                    test_union2: test_union {
                        ...on B {
                            __typename
                            common
                        }
                    }
    
                    test_enum
                    test_enum2: test_enum
                }",
            )
            .response(json! {{
                "test_interface": {
                    "__typename": "Object",
                    "foo": "bar",
                    "other": "a"
                },

                "test_interface2": {
                    "__typename": "Object2",
                    "foo": "bar",
                    "other": "a"
                },

                "test_union": {
                    "__typename": "A",
                    "common": "hello",
                    "a": "A"
                },

                "test_union2": {
                    "__typename": "B",
                    "common": "hello",
                    "b": "B"
                },

                "test_enum": "X",
                "test_enum2": "Z"
            }})
            .expected(json! {{
                "test_interface": null,
                "test_interface2": {
                    "__typename": "Object2",
                    "foo": "bar",
                },
                "test_union": null,
                "test_union2": {
                    "__typename": "B",
                    "common": "hello",
                },
                "test_enum": null,
                "test_enum2": "Z"
            }})
            .test();
    }

    #[test]
    fn fragment_on_interface_on_query() {
        let schema = r#"schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
            @link(url: "https://specs.apollo.dev/inaccessible/v0.2", for: SECURITY)
        {
            query: MyQueryObject
        }

        directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
        directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
        directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ENUM | ENUM_VALUE | SCALAR | INPUT_OBJECT | INPUT_FIELD_DEFINITION | ARGUMENT_DEFINITION

        scalar join__FieldSet
        scalar link__Import
        enum link__Purpose {
        """
        `SECURITY` features provide metadata necessary to securely resolve fields.
        """
        SECURITY

        """
        `EXECUTION` features provide metadata necessary for operation execution.
        """
        EXECUTION
        }

        enum join__Graph {
            TEST @join__graph(name: "test", url: "http://localhost:4001/graphql")
        }

        type MyQueryObject implements Interface {
            object: MyObject
            other: String
        }

        type MyObject {
            data: String
            foo: String
        }

        interface Interface {
            object: MyObject
        }"#;

        let query = "{
            ...FragmentTest
        }
        fragment FragmentTest on Interface {
            object {
                data
            }
        }";

        let schema = Schema::parse(schema, &Default::default()).expect("could not parse schema");
        let api_schema = schema.api_schema();
        let query =
            Query::parse(query, &schema, &Default::default()).expect("could not parse query");
        let mut response = Response::builder()
            .data(json! {{
                "object": {
                    "__typename": "MyObject",
                    "data": "a",
                    "foo": "bar"
                }
            }})
            .build();

        query.format_response(&mut response, None, Default::default(), api_schema);
        assert_eq_and_ordered!(
            response.data.as_ref().unwrap(),
            &json! {{
                "object": {
                    "data": "a"
                }
            }}
        );
    }

    #[test]
    fn fragment_on_interface() {
        let schema = "type Query
        {
            test_interface: Interface
        }

        interface Interface {
            foo: String
        }

        type MyTypeA implements Interface {
            foo: String
            something: String
        }

        type MyTypeB implements Interface {
            foo: String
            somethingElse: String!
        }
        ";

        FormatTest::builder()
            .schema(schema)
            .fed2()
            .query(
                "query  {
                test_interface {
                    __typename
                    foo
                    ...FragmentA
                }
            }

            fragment FragmentA on MyTypeA {
                something
            }

            fragment FragmentB on MyTypeB {
                somethingElse
            }",
            )
            .response(json! {{
                "test_interface": {
                    "__typename": "MyTypeA",
                    "foo": "bar",
                    "something": "something"
                }
            }})
            .expected(json! {{
                "test_interface": {
                    "__typename": "MyTypeA",
                    "foo": "bar",
                    "something": "something"
                }
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .fed2()
            .query(
                "query  {
                test_interface {
                    __typename
                    ...FragmentI
                }
            }

            fragment FragmentI on Interface {
                foo
            }",
            )
            .response(json! {{
                "test_interface": {
                    "__typename": "MyTypeA",
                    "foo": "bar"
                }
            }})
            .expected(json! {{
                "test_interface": {
                    "__typename": "MyTypeA",
                    "foo": "bar"
                }
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .fed2()
            .query(
                "query  {
                test_interface {
                    __typename
                    foo
                    ... on MyTypeA {
                        something
                    }
                    ... on MyTypeB {
                        somethingElse
                    }
                }
            }",
            )
            .response(json! {{
                "test_interface": {
                    "__typename": "MyTypeA",
                    "foo": "bar",
                    "something": "something"
                }
            }})
            .expected(json! {{
                "test_interface": {
                    "__typename": "MyTypeA",
                    "foo": "bar",
                    "something": "something"
                }
            }})
            .test();

        FormatTest::builder()
            .schema(schema)
            .fed2()
            .query(
                "query  {
                test_interface {
                    __typename
                    foo
                    ...FragmentB
                }
            }

            fragment FragmentA on MyTypeA {
                something
            }

            fragment FragmentB on MyTypeB {
                somethingElse
            }",
            )
            .response(json! {{
                "test_interface": {
                    "__typename": "MyTypeA",
                    "foo": "bar",
                    "something": "something"
                }
            }})
            .expected(json! {{
                "test_interface": {
                    "__typename": "MyTypeA",
                    "foo": "bar",
                }
            }})
            .test();
    }

    #[test]
    fn parse_introspection_query() {
        let schema = "type Query {
            foo: String
            stuff: Bar
            array: [Bar]
            baz: String
        }
        type Bar {
            bar: String
            baz: String
        }";

        let schema = with_supergraph_boilerplate(schema);
        let schema = Schema::parse(&schema, &Default::default()).expect("could not parse schema");
        let api_schema = schema.api_schema();

        let query = "{
            __type(name: \"Bar\") {
              name
              fields {
                name
                type {
                  name
                }
              }
            }
          }}";
        assert!(Query::parse(query, api_schema, &Default::default())
            .unwrap()
            .operations
            .get(0)
            .unwrap()
            .is_introspection());

        let query = "query {
            __schema {
              queryType {
                name
              }
            }
          }";

        assert!(Query::parse(query, api_schema, &Default::default())
            .unwrap()
            .operations
            .get(0)
            .unwrap()
            .is_introspection());

        let query = "query {
            __typename
          }";

        assert!(Query::parse(query, api_schema, &Default::default())
            .unwrap()
            .operations
            .get(0)
            .unwrap()
            .is_introspection());
    }

    #[test]
    fn fragment_on_union() {
        let schema = "type Query {
            settings: ServiceSettings
        }

        type ServiceSettings {
            location: ServiceLocation
        }

        union ServiceLocation = AccountLocation | Address

        type AccountLocation {
            id: ID
            address: Address
        }

        type Address {
            city: String
        }";

        FormatTest::builder()
            .schema(schema)
            .fed2()
            .query(
                "{
                settings {
                  location {
                    ...SettingsLocation
                  }
                }
              }

              fragment SettingsLocation on ServiceLocation {
                ... on Address {
                  city
                }
                 ... on AccountLocation {
                   id
                   address {
                     city
                   }
                 }
              }",
            )
            .response(json! {{
                "settings": {
                    "location": {
                        "__typename": "AccountLocation",
                        "id": "1234"
                    }
                }
            }})
            .expected(json! {{
                "settings": {
                    "location": {
                        "id": "1234",
                        "address": null
                    }
                }
            }})
            .test();
    }

    #[test]
    fn fragment_on_interface_without_typename() {
        let schema = "type Query {
            inStore(key: String!): InStore!
        }

        type InStore implements CartQueryInterface {
            cart: Cart
            carts: CartQueryResult!
        }

        interface CartQueryInterface {
            carts: CartQueryResult!
            cart: Cart
        }

        type Cart {
            id: ID!
            total: Int!
        }

        type CartQueryResult {
            results: [Cart!]!
            total: Int!
        }";

        FormatTest::builder()
            .schema(schema)
            .fed2()
            .query(
                r#"query {
                    mtb: inStore(key: "mountainbikes") {
                        ...CartFragmentTest
                    }
                }
    
                fragment CartFragmentTest on CartQueryInterface {
                    carts {
                        results {
                            id
                        }
                        total
                    }
                }"#,
            )
            .response(json! {{
                "mtb": {
                    "carts": {
                        "results": [{"id": "id"}],
                        "total": 1234
                    },
                    "cart": null
                }
            }})
            .expected(json! {{
                "mtb": {
                    "carts": {
                        "results": [{"id": "id"}],
                        "total": 1234
                    },
                }
            }})
            .test();

        // With inline fragment
        FormatTest::builder()
            .schema(schema)
            .fed2()
            .query(
                r#"query {
                mtb: inStore(key: "mountainbikes") {
                    ... on CartQueryInterface {
                        carts {
                            results {
                                id
                            }
                            total
                        }
                    }
                }
            }"#,
            )
            .response(json! {{
                "mtb": {
                    "carts": {
                        "results": [{"id": "id"}],
                        "total": 1234
                    },
                    "cart": null
                }
            }})
            .expected(json! {{
                "mtb": {
                    "carts": {
                        "results": [{"id": "id"}],
                        "total": 1234
                    },
                }
            }})
            .test();
    }

    #[test]
    fn query_operation_nullification() {
        FormatTest::builder()
            .schema(
                "type Query {
                get: Thing
            }
            type Thing {
                name: String
            }
            ",
            )
            .query(
                "{
                    get {
                        name
                    }
                }",
            )
            .response(json! {{ }})
            .expected(json! {{
                "get": null,
            }})
            .test();

        FormatTest::builder()
            .schema(
                "type Query {
                    get: Thing
                }
                type Thing {
                    name: String
                }",
            )
            .query(
                "query {
                    ...F
                 }
                 
                 fragment F on Query {
                     get {
                         name
                     }
                 }",
            )
            .response(json! {{ }})
            .expected(json! {{
                "get": null,
            }})
            .test();

        FormatTest::builder()
            .schema(
                "type Query {
                get: Thing
            }
            type Thing {
                name: String
            }",
            )
            .query(
                "query {
                ... on Query {
                 get {
                     name
                 }
                }
             }",
            )
            .response(json! {{ }})
            .expected(json! {{
                "get": null,
            }})
            .test();

        FormatTest::builder()
            .schema(
                "type Query {
                get: Thing!
            }
            type Thing {
                name: String
            }",
            )
            .query(
                "{
                get {
                    name
                }
            }",
            )
            .response(json! {{ }})
            .expected(Value::Null)
            .test();
    }
}
