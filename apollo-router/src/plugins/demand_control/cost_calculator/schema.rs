use std::ops::Deref;
use std::sync::Arc;

use ahash::HashMap;
use ahash::HashMapExt;
use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_federation::link::cost_spec_definition::CostDirective;
use apollo_federation::link::cost_spec_definition::CostSpecDefinition;
use apollo_federation::link::cost_spec_definition::ListSizeDirective;
use apollo_federation::schema::ValidFederationSchema;

use crate::plugins::demand_control::DemandControlError;
use crate::plugins::demand_control::cost_calculator::directives::RequiresDirective;

pub(in crate::plugins::demand_control) struct InputDefinition {
    name: Name,
    ty: ExtendedType,
    cost_directive: Option<CostDirective>,
}

impl InputDefinition {
    fn new(
        schema: &ValidFederationSchema,
        field_definition: &InputValueDefinition,
    ) -> Result<Self, DemandControlError> {
        let field_type = schema
            .schema()
            .types
            .get(field_definition.ty.inner_named_type())
            .ok_or_else(|| {
                DemandControlError::QueryParseFailure(format!(
                    "Field {} was found in query, but its type is missing from the schema.",
                    field_definition.name
                ))
            })?;
        let processed_inputs = InputDefinition {
            name: field_definition.name.clone(),
            ty: field_type.clone(),
            cost_directive: CostSpecDefinition::cost_directive_from_argument(
                schema,
                field_definition,
                field_type,
            )?,
        };

        Ok(processed_inputs)
    }

    pub(in crate::plugins::demand_control) fn name(&self) -> &Name {
        &self.name
    }

    pub(in crate::plugins::demand_control) fn ty(&self) -> &ExtendedType {
        &self.ty
    }

    pub(in crate::plugins::demand_control) fn cost_directive(&self) -> Option<&CostDirective> {
        self.cost_directive.as_ref()
    }
}

pub(in crate::plugins::demand_control) struct FieldDefinition {
    ty: ExtendedType,
    cost_directive: Option<CostDirective>,
    list_size_directive: Option<ListSizeDirective>,
    requires_directive: Option<RequiresDirective>,
    arguments: HashMap<Name, InputDefinition>,
}

impl FieldDefinition {
    fn new(
        schema: &ValidFederationSchema,
        parent_type_name: &Name,
        field_definition: &apollo_compiler::ast::FieldDefinition,
    ) -> Result<Self, DemandControlError> {
        let field_type = schema
            .schema()
            .types
            .get(field_definition.ty.inner_named_type())
            .ok_or_else(|| {
                DemandControlError::QueryParseFailure(format!(
                    "Field {} was found in query, but its type is missing from the schema.",
                    field_definition.name,
                ))
            })?;
        let mut processed_field_definition = Self {
            ty: field_type.clone(),
            cost_directive: None,
            list_size_directive: None,
            requires_directive: None,
            arguments: HashMap::new(),
        };

        processed_field_definition.cost_directive =
            CostSpecDefinition::cost_directive_from_field(schema, field_definition, field_type)?;
        processed_field_definition.list_size_directive =
            CostSpecDefinition::list_size_directive_from_field_definition(
                schema,
                field_definition,
            )?;
        processed_field_definition.requires_directive = RequiresDirective::from_field_definition(
            field_definition,
            parent_type_name,
            schema.schema(),
        )?;

        for argument in &field_definition.arguments {
            processed_field_definition.arguments.insert(
                argument.name.clone(),
                InputDefinition::new(schema, argument)?,
            );
        }

        Ok(processed_field_definition)
    }

    pub(in crate::plugins::demand_control) fn ty(&self) -> &ExtendedType {
        &self.ty
    }

    pub(in crate::plugins::demand_control) fn cost_directive(&self) -> Option<&CostDirective> {
        self.cost_directive.as_ref()
    }

    pub(in crate::plugins::demand_control) fn list_size_directive(
        &self,
    ) -> Option<&ListSizeDirective> {
        self.list_size_directive.as_ref()
    }

    pub(in crate::plugins::demand_control) fn requires_directive(
        &self,
    ) -> Option<&RequiresDirective> {
        self.requires_directive.as_ref()
    }

    pub(in crate::plugins::demand_control) fn argument_by_name(
        &self,
        argument_name: &str,
    ) -> Option<&InputDefinition> {
        self.arguments.get(argument_name)
    }
}

pub(crate) struct DemandControlledSchema {
    inner: ValidFederationSchema,
    input_field_definitions: HashMap<Name, HashMap<Name, InputDefinition>>,
    output_field_definitions: HashMap<Name, HashMap<Name, FieldDefinition>>,
}

impl DemandControlledSchema {
    pub(crate) fn new(schema: Arc<Valid<Schema>>) -> Result<Self, DemandControlError> {
        let fed_schema = ValidFederationSchema::new((*schema).clone())?;
        let mut input_field_definitions: HashMap<Name, HashMap<Name, InputDefinition>> =
            HashMap::with_capacity(schema.types.len());
        let mut output_field_definitions: HashMap<Name, HashMap<Name, FieldDefinition>> =
            HashMap::with_capacity(schema.types.len());

        for (type_name, type_) in &schema.types {
            match type_ {
                ExtendedType::Interface(ty) => {
                    let type_fields = output_field_definitions
                        .entry(type_name.clone())
                        .or_insert_with(|| HashMap::with_capacity(ty.fields.len()));
                    for (field_name, field_definition) in &ty.fields {
                        type_fields.insert(
                            field_name.clone(),
                            FieldDefinition::new(&fed_schema, type_name, field_definition)?,
                        );
                    }
                }
                ExtendedType::Object(ty) => {
                    let type_fields = output_field_definitions
                        .entry(type_name.clone())
                        .or_insert_with(|| HashMap::with_capacity(ty.fields.len()));
                    for (field_name, field_definition) in &ty.fields {
                        type_fields.insert(
                            field_name.clone(),
                            FieldDefinition::new(&fed_schema, type_name, field_definition)?,
                        );
                    }
                }
                ExtendedType::InputObject(ty) => {
                    let type_fields = input_field_definitions
                        .entry(type_name.clone())
                        .or_insert_with(|| HashMap::with_capacity(ty.fields.len()));
                    for (field_name, field_definition) in &ty.fields {
                        type_fields.insert(
                            field_name.clone(),
                            InputDefinition::new(&fed_schema, field_definition)?,
                        );
                    }
                }
                _ => {
                    // Other types don't have fields
                }
            }
        }

        input_field_definitions.shrink_to_fit();
        output_field_definitions.shrink_to_fit();

        Ok(Self {
            inner: fed_schema,
            input_field_definitions,
            output_field_definitions,
        })
    }

    pub(crate) fn empty(schema: Arc<Valid<Schema>>) -> Result<Self, DemandControlError> {
        let fed_schema = ValidFederationSchema::new((*schema).clone())?;
        Ok(Self {
            inner: fed_schema,
            input_field_definitions: Default::default(),
            output_field_definitions: Default::default(),
        })
    }

    pub(in crate::plugins::demand_control) fn input_field_definition(
        &self,
        type_name: &str,
        field_name: &str,
    ) -> Option<&InputDefinition> {
        self.input_field_definitions.get(type_name)?.get(field_name)
    }

    pub(in crate::plugins::demand_control) fn output_field_definition(
        &self,
        type_name: &str,
        field_name: &str,
    ) -> Option<&FieldDefinition> {
        self.output_field_definitions
            .get(type_name)?
            .get(field_name)
    }
}

impl AsRef<Valid<Schema>> for DemandControlledSchema {
    fn as_ref(&self) -> &Valid<Schema> {
        self.inner.schema()
    }
}

impl Deref for DemandControlledSchema {
    type Target = Schema;

    fn deref(&self) -> &Self::Target {
        self.inner.schema()
    }
}
