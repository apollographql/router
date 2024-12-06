use std::sync::Arc;

use ahash::HashMap;
use ahash::HashMapExt;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::executable;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_federation::link::cost_spec_definition::CostDirective;
use apollo_federation::link::cost_spec_definition::CostSpecDefinition;
use apollo_federation::link::cost_spec_definition::ListSizeDirective;
use apollo_federation::schema::ValidFederationSchema;

use super::directives::ListSizeDirective as ExecutableListSizeDirective;
use crate::graphql::Response;
use crate::json_ext::Object;
use crate::plugins::demand_control::DemandControlError;

pub(crate) struct DemandControlledSchema {
    mutation: Option<Operation>,
    query: Option<Operation>,
    subscription: Option<Operation>,
}

pub(crate) struct InputObject {
    name: Name,
    cost_directive: Option<CostDirective>,
    fields: HashMap<Name, InputType>,
}

impl InputObject {
    fn new(
        schema: &Arc<ValidFederationSchema>,
        arg_def: &InputValueDefinition,
        ty: &ExtendedType,
    ) -> Result<Self, DemandControlError> {
        tracing::debug!(
            "Creating InputObjectType instance for {}: {}",
            arg_def.name,
            ty.name()
        );
        let cost_directive = CostSpecDefinition::cost_directive_from_argument(schema, arg_def, ty)?;
        let mut input_object = Self {
            name: arg_def.name.clone(),
            cost_directive,
            fields: Default::default(),
        };

        match ty {
            ExtendedType::InputObject(obj) => {
                for (name, def) in &obj.fields {
                    let inner_ty = schema
                        .schema()
                        .types
                        .get(def.ty.inner_named_type())
                        .ok_or_else(|| DemandControlError::ArgumentLookupError {
                            field_name: arg_def.name.to_string(),
                            arg_name: name.to_string(),
                        })?;
                    input_object.fields.insert(
                        name.clone(),
                        InputType::try_from_input_value_definition(schema, def, inner_ty)?,
                    );
                }
            }
            _ => todo!("not allowed"),
        }

        Ok(input_object)
    }

    fn score_input_object(
        &self,
        ctx: &ScoringContext,
        obj: &apollo_compiler::ast::Value,
    ) -> Result<f64, DemandControlError> {
        match obj {
            executable::Value::Null => Ok(0.0),
            executable::Value::Enum(_)
            | executable::Value::String(_)
            | executable::Value::Float(_)
            | executable::Value::Int(_)
            | executable::Value::Boolean(_)
            | executable::Value::List(_) => todo!("scalars shouldn't be allowed here"),
            executable::Value::Object(fields) => {
                let mut cost = self
                    .cost_directive
                    .as_ref()
                    .map_or(1.0, |cost| cost.weight());
                for (name, value) in fields {
                    cost += self
                        .fields
                        .get(name)
                        .ok_or_else(|| DemandControlError::FieldLookupError {
                            type_name: "TBD".to_string(), // TODO: type name
                            field_name: name.to_string(),
                        })?
                        .score_input_object(ctx, value)?;
                }
                tracing::debug!("Input {} cost {}: {:?}", self.name, cost, obj);
                Ok(cost)
            }
            executable::Value::Variable(name) => {
                // We make a best effort attempt to score the variable, but some of these may not exist in the variables
                // sent on the supergraph request, such as `$representations`.
                if let Some(variable) = ctx.variables.get(name.as_str()) {
                    let cost = self.score_variable(ctx, variable)?;
                    tracing::debug!("Variable {} cost {}, {:?}", name, cost, variable);
                    Ok(cost)
                } else {
                    Ok(0.0)
                }
            }
        }
    }

    fn score_variable(
        &self,
        ctx: &ScoringContext,
        variable_value: &serde_json_bytes::Value,
    ) -> Result<f64, DemandControlError> {
        let mut cost = 1.0; // TODO: This default cost should be factored out
        if let serde_json_bytes::Value::Object(fields) = variable_value {
            for (k, v) in fields {
                cost += self
                    .fields
                    .get(k.as_str())
                    .ok_or_else(|| DemandControlError::FieldLookupError {
                        type_name: "TBD".to_string(),
                        field_name: k.as_str().to_string(),
                    })?
                    .score_variable(ctx, v)?;
            }
        } else {
            todo!("Other types aren't allowed here")
        }
        Ok(cost)
    }
}

pub(crate) struct ScalarField {
    name: Name,
    cost_directive: Option<CostDirective>,
    args: HashMap<Name, InputType>,
}

impl ScalarField {
    fn new(
        schema: &Arc<ValidFederationSchema>,
        field: &FieldDefinition,
        ty: &ExtendedType,
    ) -> Result<Self, DemandControlError> {
        tracing::debug!(
            "Creating ScalarType instance for {}: {}",
            field.name,
            ty.name()
        );
        let mut scalar = Self {
            name: field.name.clone(),
            cost_directive: None,
            args: HashMap::with_capacity(field.arguments.len()),
        };
        scalar.cost_directive = CostSpecDefinition::cost_directive_from_field(schema, field, ty)?;

        for arg_def in &field.arguments {
            let arg_ty = schema
                .schema()
                .types
                .get(arg_def.ty.inner_named_type())
                .ok_or_else(|| DemandControlError::ArgumentLookupError {
                    field_name: field.name.to_string(),
                    arg_name: arg_def.name.to_string(),
                })?;
            let arg_node = InputType::try_from_input_value_definition(schema, arg_def, arg_ty)?;
            scalar.args.insert(arg_def.name.clone(), arg_node);
        }

        Ok(scalar)
    }

    fn score_field(
        &self,
        ctx: &ScoringContext,
        field: &executable::Field,
        list_size_from_upstream: Option<i32>,
    ) -> Result<f64, DemandControlError> {
        // TODO: Handle type  mismatch?
        let type_cost = self
            .cost_directive
            .as_ref()
            .map_or(0.0, |cost| cost.weight());

        let instance_count = list_size_from_upstream.unwrap_or(1);

        let mut arguments_cost = 0.0;
        for argument in &field.arguments {
            if let Some(arg_ty) = self.args.get(&argument.name) {
                arguments_cost += arg_ty.score_input_object(ctx, &argument.value)?;
            } else {
                return Err(DemandControlError::ArgumentLookupError {
                    field_name: field.name.to_string(),
                    arg_name: argument.name.to_string(),
                });
            }
        }
        tracing::debug!(
            "Field {} cost breakdown: (type cost) {} * (count) {} + (arguments cost) {}",
            field.name,
            type_cost,
            instance_count,
            arguments_cost
        );
        Ok(type_cost + arguments_cost)
    }

    fn score_response(&self) -> Result<f64, DemandControlError> {
        let cost = self
            .cost_directive
            .as_ref()
            .map_or(0.0, |cost| cost.weight());
        tracing::debug!("Scalar {} cost {} in response", self.name, cost);
        Ok(cost)
    }
}

struct ScalarInput {
    name: Name,
    cost_directive: Option<CostDirective>,
}

impl ScalarInput {
    fn new(
        schema: &Arc<ValidFederationSchema>,
        input: &InputValueDefinition,
        ty: &ExtendedType,
    ) -> Result<Self, DemandControlError> {
        tracing::debug!(
            "Creating ScalarType instance for {}: {}",
            input.name,
            ty.name()
        );
        let cost_directive = CostSpecDefinition::cost_directive_from_argument(schema, input, ty)?;
        Ok(Self {
            name: ty.name().clone(),
            cost_directive,
        })
    }

    fn score_ast_value(&self) -> Result<f64, DemandControlError> {
        Ok(self
            .cost_directive
            .as_ref()
            .map_or(0.0, |cost| cost.weight()))
    }

    // TODO: This can be reused for variables, so probably deserves a generic name like score_json
    fn score_response(&self) -> Result<f64, DemandControlError> {
        let cost = self
            .cost_directive
            .as_ref()
            .map_or(0.0, |cost| cost.weight());
        tracing::debug!("Scalar {} cost {} in response", self.name, cost);
        Ok(cost)
    }
}

enum InputType {
    InputObject(InputObject),
    Scalar(ScalarInput),
}

impl InputType {
    fn try_from_input_value_definition(
        schema: &Arc<ValidFederationSchema>,
        arg_def: &InputValueDefinition,
        ty: &ExtendedType,
    ) -> Result<Self, DemandControlError> {
        tracing::debug!(
            "Creating InputType instance for {}: {}",
            arg_def.name,
            ty.name()
        );
        match ty {
            ExtendedType::InputObject(_) => {
                InputObject::new(schema, arg_def, ty).map(InputType::InputObject)
            }
            ExtendedType::Scalar(_) | ExtendedType::Enum(_) => {
                ScalarInput::new(schema, arg_def, ty).map(InputType::Scalar)
            }
            _ => Err(DemandControlError::QueryParseFailure(format!(
                "Type {} is not allowed as an input object",
                ty.name()
            ))),
        }
    }

    fn score_input_object(
        &self,
        ctx: &ScoringContext,
        val: &apollo_compiler::ast::Value,
    ) -> Result<f64, DemandControlError> {
        match self {
            InputType::InputObject(input_object_type) => {
                input_object_type.score_input_object(ctx, val)
            }
            InputType::Scalar(scalar_type) => scalar_type.score_ast_value(),
        }
    }

    fn score_variable(
        &self,
        ctx: &ScoringContext,
        variable_value: &serde_json_bytes::Value,
    ) -> Result<f64, DemandControlError> {
        match self {
            InputType::InputObject(input_object) => {
                input_object.score_variable(ctx, variable_value)
            }
            InputType::Scalar(scalar_input) => scalar_input.score_response(),
        }
    }
}

pub(crate) struct CompositeField {
    name: Name,
    cost_directive: Option<CostDirective>,
    list_size_directive: Option<ListSizeDirective>,
    args: HashMap<Name, InputType>,
    fields: HashMap<Name, OutputType>,
}

impl CompositeField {
    fn new(
        schema: &Arc<ValidFederationSchema>,
        definition: &FieldDefinition,
        ty: &ExtendedType,
    ) -> Result<Self, DemandControlError> {
        tracing::debug!(
            "Creating CompositeField instance for {}: {}",
            definition.name,
            ty.name()
        );
        let cost_directive = CostSpecDefinition::cost_directive_from_field(schema, definition, ty)?;
        let list_size_directive =
            CostSpecDefinition::list_size_directive_from_field_definition(schema, definition)?;
        // TODO: let requires_directive = RequiresDirective::from_field_definition(field_definition, parent_type_name, schema)

        let mut obj = Self {
            name: definition.name.clone(),
            cost_directive,
            list_size_directive,
            // TODO: requires
            args: HashMap::new(),
            fields: HashMap::new(),
        };

        for arg_def in &definition.arguments {
            let arg_ty = schema
                .schema()
                .types
                .get(arg_def.ty.inner_named_type())
                .ok_or_else(|| DemandControlError::ArgumentLookupError {
                    field_name: definition.name.to_string(),
                    arg_name: arg_def.name.to_string(),
                })?;
            let arg_node = InputType::try_from_input_value_definition(schema, arg_def, arg_ty)?;
            obj.args.insert(arg_def.name.clone(), arg_node);
        }

        // TODO: Dedup
        match ty {
            ExtendedType::Object(obj_ty) => {
                for (child_field_name, child_field_def) in &obj_ty.fields {
                    let child_field_ty = schema
                        .schema()
                        .types
                        .get(child_field_def.ty.inner_named_type())
                        .ok_or_else(|| DemandControlError::FieldLookupError {
                            type_name: ty.name().to_string(),
                            field_name: child_field_name.to_string(),
                        })?;
                    obj.fields.insert(
                        child_field_name.clone(),
                        OutputType::new(schema, child_field_def, child_field_ty)?,
                    );
                }
            }
            ExtendedType::Interface(itf_ty) => {
                for (child_field_name, child_field_def) in &itf_ty.fields {
                    let child_field_ty = schema
                        .schema()
                        .types
                        .get(child_field_def.ty.inner_named_type())
                        .ok_or_else(|| DemandControlError::FieldLookupError {
                            type_name: ty.name().to_string(),
                            field_name: child_field_name.to_string(),
                        })?;
                    obj.fields.insert(
                        child_field_name.clone(),
                        OutputType::new(schema, child_field_def, child_field_ty)?,
                    );
                }
            }
            // TODO: Union?
            _ => {}
        };

        Ok(obj)
    }

    fn score_field(
        &self,
        ctx: &ScoringContext,
        field: &executable::Field,
        list_size_from_upstream: Option<i32>,
    ) -> Result<f64, DemandControlError> {
        let list_size_directive = match self.list_size_directive.as_ref() {
            Some(dir) => ExecutableListSizeDirective::new(dir, field, ctx.variables).map(Some),
            None => Ok(None),
        }?;
        let instance_count = if !field.ty().is_list() {
            1
        } else if let Some(value) = list_size_from_upstream {
            // This is a sized field whose length is defined by the `@listSize` directive on the parent field
            value
        } else if let Some(expected_size) = list_size_directive
            .as_ref()
            .and_then(|dir| dir.expected_size)
        {
            expected_size
        } else {
            ctx.default_list_size as i32
        };

        let mut type_cost = self
            .cost_directive
            .as_ref()
            .map_or(1.0, |cost| cost.weight());
        type_cost +=
            self.score_selection_set(ctx, &field.selection_set, list_size_directive.as_ref())?;

        let mut arguments_cost = 0.0;
        for argument in &field.arguments {
            if let Some(arg_ty) = self.args.get(&argument.name) {
                arguments_cost += arg_ty.score_input_object(ctx, &argument.value)?;
            } else {
                return Err(DemandControlError::ArgumentLookupError {
                    field_name: field.name.to_string(),
                    arg_name: argument.name.to_string(),
                });
            }
        }

        // TODO: Requirements
        let requirements_cost = 0.0;

        let cost = (instance_count as f64) * type_cost + arguments_cost + requirements_cost;
        tracing::debug!(
            "Field {} cost breakdown: (count) {} * (type cost) {} + (arguments) {} + (requirements) {} = {}",
            field.name,
            instance_count,
            type_cost,
            arguments_cost,
            requirements_cost,
            cost
        );

        Ok(cost)
    }

    fn score_selection(
        &self,
        ctx: &ScoringContext,
        selection: &executable::Selection,
        parent_list_size_directive: Option<&ExecutableListSizeDirective>,
    ) -> Result<f64, DemandControlError> {
        match selection {
            executable::Selection::Field(field) => {
                if let Some(field_ty) = self.fields.get(&field.name) {
                    field_ty.score_field(
                        ctx,
                        field,
                        parent_list_size_directive.and_then(|dir| dir.size_of(field)),
                    )
                } else {
                    Err(DemandControlError::FieldLookupError {
                        type_name: self.name.to_string(),
                        field_name: field.name.to_string(),
                    })
                }
            }
            executable::Selection::FragmentSpread(fragment_spread) => {
                if let Some(fragment) = fragment_spread.fragment_def(ctx.query) {
                    self.score_selection_set(
                        ctx,
                        &fragment.selection_set,
                        parent_list_size_directive,
                    )
                } else {
                    Err(DemandControlError::QueryParseFailure(format!(
                        "Parsed operation did not have a definition for fragment {}",
                        fragment_spread.fragment_name
                    )))
                }
            }
            executable::Selection::InlineFragment(inline_fragment) => self.score_selection_set(
                ctx,
                &inline_fragment.selection_set,
                parent_list_size_directive,
            ),
        }
    }

    fn score_selection_set(
        &self,
        ctx: &ScoringContext,
        selection_set: &executable::SelectionSet,
        parent_list_size_directive: Option<&ExecutableListSizeDirective>,
    ) -> Result<f64, DemandControlError> {
        let mut cost = 0.0;
        for selection in &selection_set.selections {
            cost += self.score_selection(ctx, selection, parent_list_size_directive)?
        }
        Ok(cost)
    }

    fn score_response(
        &self,
        ctx: &ScoringContext,
        value: &serde_json_bytes::Value,
    ) -> Result<f64, DemandControlError> {
        let mut cost = 1.0;
        if let serde_json_bytes::Value::Object(fields) = value {
            for (k, v) in fields {
                self.fields
                    .get(k.as_str())
                    .ok_or_else(|| DemandControlError::FieldLookupError {
                        type_name: "TBD".to_string(),
                        field_name: k.as_str().to_string(),
                    })?
                    .score_response(ctx, v)?;
            }
        } else if let serde_json_bytes::Value::Array(items) = value {
            for item in items {
                cost += self.score_response(ctx, item)?;
            }
        }
        // TODO: Need to account for argument cost here
        Ok(cost)
    }
}

pub(crate) enum OutputType {
    Composite(CompositeField),
    Scalar(ScalarField),
}

impl OutputType {
    fn new(
        schema: &Arc<ValidFederationSchema>,
        definition: &FieldDefinition,
        ty: &ExtendedType,
    ) -> Result<Self, DemandControlError> {
        match ty {
            ExtendedType::Object(_) | ExtendedType::Interface(_) | ExtendedType::Union(_) => {
                CompositeField::new(schema, definition, ty).map(OutputType::Composite)
            }
            ExtendedType::Enum(_) | ExtendedType::Scalar(_) => {
                ScalarField::new(schema, definition, ty).map(OutputType::Scalar)
            }
            ExtendedType::InputObject(_) => {
                todo!("This is not allowed")
            }
        }
    }

    fn score_field(
        &self,
        ctx: &ScoringContext,
        field: &executable::Field,
        list_size_from_upstream: Option<i32>,
    ) -> Result<f64, DemandControlError> {
        match self {
            OutputType::Composite(composite_type) => {
                composite_type.score_field(ctx, field, list_size_from_upstream)
            }
            OutputType::Scalar(scalar_type) => {
                scalar_type.score_field(ctx, field, list_size_from_upstream)
            }
        }
    }

    fn score_response(
        &self,
        ctx: &ScoringContext,
        value: &serde_json_bytes::Value,
    ) -> Result<f64, DemandControlError> {
        match self {
            OutputType::Composite(composite_type) => composite_type.score_response(ctx, value),
            OutputType::Scalar(scalar_type) => scalar_type.score_response(),
        }
    }
}

struct Operation {
    fields: HashMap<Name, OutputType>,
}

impl Operation {
    fn new(
        schema: &Arc<ValidFederationSchema>,
        ty: &ExtendedType,
    ) -> Result<Self, DemandControlError> {
        let mut op = Operation {
            fields: HashMap::new(),
        };
        match ty {
            ExtendedType::Object(obj) => {
                for (field_name, field_def) in &obj.fields {
                    let field_type = schema
                        .schema()
                        .types
                        .get(field_def.ty.inner_named_type())
                        .ok_or_else(|| DemandControlError::FieldLookupError {
                            type_name: ty.name().to_string(),
                            field_name: field_name.to_string(),
                        })?;
                    op.fields.insert(
                        field_name.clone(),
                        OutputType::new(schema, field_def, field_type)?,
                    );
                }
            }
            ExtendedType::Interface(itf) => {
                for (field_name, field_def) in &itf.fields {
                    let field_type = schema
                        .schema()
                        .types
                        .get(field_def.ty.inner_named_type())
                        .ok_or_else(|| DemandControlError::FieldLookupError {
                            type_name: ty.name().to_string(),
                            field_name: field_name.to_string(),
                        })?;
                    op.fields.insert(
                        field_name.clone(),
                        OutputType::new(schema, field_def, field_type)?,
                    );
                }
            }
            ExtendedType::Union(_) => todo!(),
            _ => todo!(),
        }
        Ok(op)
    }

    fn score(
        &self,
        ctx: &ScoringContext,
        op: &executable::Operation, // TODO: Just embed this in the schema type
    ) -> Result<f64, DemandControlError> {
        let mut cost = if op.is_mutation() { 10.0 } else { 0.0 };
        cost += self.score_selection_set(ctx, &op.selection_set)?;
        Ok(cost)
    }

    fn score_selection_set(
        &self,
        ctx: &ScoringContext,
        selection_set: &executable::SelectionSet,
    ) -> Result<f64, DemandControlError> {
        let mut cost = 0.0;
        for selection in &selection_set.selections {
            cost += self.score_selection(ctx, selection)?;
        }
        Ok(cost)
    }

    fn score_selection(
        &self,
        ctx: &ScoringContext,
        selection: &executable::Selection,
    ) -> Result<f64, DemandControlError> {
        match selection {
            executable::Selection::Field(field) => {
                if let Some(field_ty) = self.fields.get(&field.name) {
                    // Operations don't have listSize, so pass None
                    field_ty.score_field(ctx, field, None)
                } else {
                    Err(DemandControlError::FieldLookupError {
                        type_name: "Root operation".to_string(),
                        field_name: field.name.to_string(),
                    })
                }
            }
            executable::Selection::FragmentSpread(fragment_spread) => {
                if let Some(fragment) = fragment_spread.fragment_def(ctx.query) {
                    self.score_selection_set(ctx, &fragment.selection_set)
                } else {
                    Err(DemandControlError::QueryParseFailure(format!(
                        "Parsed operation did not have a definition for fragment {}",
                        fragment_spread.fragment_name
                    )))
                }
            }
            executable::Selection::InlineFragment(inline_fragment) => {
                self.score_selection_set(ctx, &inline_fragment.selection_set)
            }
        }
    }

    fn score_response(
        &self,
        ctx: &ScoringContext,
        op: &executable::Operation,
        value: &serde_json_bytes::Value,
    ) -> Result<f64, DemandControlError> {
        let mut cost = if op.is_mutation() { 10.0 } else { 0.0 };
        if let serde_json_bytes::Value::Object(fields) = value {
            for (k, v) in fields {
                cost += self
                    .fields
                    .get(k.as_str())
                    .ok_or_else(|| DemandControlError::FieldLookupError {
                        type_name: "Query".to_string(),
                        field_name: k.as_str().to_string(),
                    })?
                    .score_response(ctx, v)?;
            }
        }
        Ok(cost)
    }
}

struct ScoringContext<'a> {
    query: &'a ExecutableDocument,
    variables: &'a Object,
    default_list_size: u32,
}

impl DemandControlledSchema {
    pub(crate) fn new(schema: Arc<Valid<Schema>>) -> Result<Self, DemandControlError> {
        let fed_schema = Arc::new(ValidFederationSchema::new((*schema).clone())?);
        let mutation = if let Some(mutation_type) = schema
            .schema_definition
            .mutation
            .as_ref()
            .and_then(|name| schema.types.get(name.as_str()))
        {
            tracing::debug!("Creating root mutation");
            Some(Operation::new(&fed_schema, mutation_type)?)
        } else {
            None
        };
        let query = if let Some(query_type) = schema
            .schema_definition
            .query
            .as_ref()
            .and_then(|name| schema.types.get(name.as_str()))
        {
            tracing::debug!("Creating root query");
            Some(Operation::new(&fed_schema, query_type)?)
        } else {
            None
        };
        let subscription = if let Some(subscription_type) = schema
            .schema_definition
            .subscription
            .as_ref()
            .and_then(|name| schema.types.get(name.as_str()))
        {
            tracing::debug!("Creating root subscription");
            Some(Operation::new(&fed_schema, subscription_type)?)
        } else {
            None
        };

        Ok(Self {
            mutation,
            query,
            subscription,
        })
    }

    pub(crate) fn score_request(
        &self,
        query: &ExecutableDocument,
        variables: &Object,
        default_list_size: u32,
    ) -> Result<f64, DemandControlError> {
        let mut cost = 0.0;
        let ctx = ScoringContext {
            query,
            variables,
            default_list_size,
        };
        if let Some(op) = &query.operations.anonymous {
            cost += self.score_operation(&ctx, op)?;
        }
        for (_name, op) in query.operations.named.iter() {
            cost += self.score_operation(&ctx, op)?;
        }
        Ok(cost)
    }

    fn score_operation(
        &self,
        ctx: &ScoringContext,
        op: &executable::Operation,
    ) -> Result<f64, DemandControlError> {
        self.get_operation(op)?.score(ctx, op)
    }

    fn get_operation(&self, op: &executable::Operation) -> Result<&Operation, DemandControlError> {
        let operation = match op.operation_type {
            executable::OperationType::Query => &self.query,
            executable::OperationType::Mutation => &self.mutation,
            executable::OperationType::Subscription => &self.subscription,
        };
        operation
            .as_ref()
            .ok_or_else(|| DemandControlError::QueryParseFailure("TBD".to_string()))
    }

    pub(crate) fn score_response(
        &self,
        query: &ExecutableDocument,
        response: &Response,
        variables: &Object,
        default_list_size: u32,
    ) -> Result<f64, DemandControlError> {
        if let Some(value) = &response.data {
            let mut cost = 0.0;
            let ctx = ScoringContext {
                query,
                variables,
                default_list_size,
            };
            if let Some(op) = &query.operations.anonymous {
                cost += self.score_operation_response(&ctx, op, value)?;
            }
            for (_name, op) in query.operations.named.iter() {
                cost += self.score_operation_response(&ctx, op, value)?;
            }
            Ok(cost)
        } else {
            Ok(0.0)
        }
    }

    fn score_operation_response(
        &self,
        ctx: &ScoringContext,
        op: &executable::Operation,
        value: &serde_json_bytes::Value,
    ) -> Result<f64, DemandControlError> {
        let operation = self.get_operation(op)?;
        operation.score_response(ctx, op, value)
    }
}
