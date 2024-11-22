use std::ops::Deref;
use std::sync::Arc;

use ahash::HashMap;
use ahash::HashMapExt;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_federation::link::cost_spec_definition::CostDirective;
use apollo_federation::link::cost_spec_definition::CostSpecDefinition;
use apollo_federation::link::cost_spec_definition::ListSizeDirective;

use super::directives::RequiresDirective;
use crate::plugins::demand_control::DemandControlError;

pub(crate) struct DemandControlledSchema {
    pub(crate) inner: Arc<Valid<Schema>>,
    type_field_cost_directives: HashMap<Name, HashMap<Name, CostDirective>>,
    type_field_list_size_directives: HashMap<Name, HashMap<Name, ListSizeDirective>>,
    type_field_requires_directives: HashMap<Name, HashMap<Name, RequiresDirective>>,
}

impl DemandControlledSchema {
    pub(crate) fn new(schema: Arc<Valid<Schema>>) -> Result<Self, DemandControlError> {
        let mut type_field_cost_directives: HashMap<Name, HashMap<Name, CostDirective>> =
            HashMap::new();
        let mut type_field_list_size_directives: HashMap<Name, HashMap<Name, ListSizeDirective>> =
            HashMap::new();
        let mut type_field_requires_directives: HashMap<Name, HashMap<Name, RequiresDirective>> =
            HashMap::new();

        for (type_name, type_) in &schema.types {
            let field_cost_directives = type_field_cost_directives
                .entry(type_name.clone())
                .or_default();
            let field_list_size_directives = type_field_list_size_directives
                .entry(type_name.clone())
                .or_default();
            let field_requires_directives = type_field_requires_directives
                .entry(type_name.clone())
                .or_default();

            match type_ {
                ExtendedType::Interface(ty) => {
                    for field_name in ty.fields.keys() {
                        let field_definition = schema.type_field(type_name, field_name)?;
                        let field_type = schema.types.get(field_definition.ty.inner_named_type()).ok_or_else(|| {
                            DemandControlError::QueryParseFailure(format!(
                                "Field {} was found in query, but its type is missing from the schema.",
                                field_name
                            ))
                        })?;

                        if let Some(cost_directive) = CostSpecDefinition::cost_directive_from_field(
                            &schema,
                            field_definition,
                            field_type,
                        ) {
                            field_cost_directives.insert(field_name.clone(), cost_directive);
                        }

                        if let Some(list_size_directive) =
                            CostSpecDefinition::list_size_directive_from_field_definition(
                                &schema,
                                field_definition,
                            )
                        {
                            field_list_size_directives
                                .insert(field_name.clone(), list_size_directive);
                        }

                        // TODO: Need to handle renaming for @requires also
                        if let Some(requires_directive) = RequiresDirective::from_field_definition(
                            field_definition,
                            type_name,
                            &schema,
                        )? {
                            field_requires_directives
                                .insert(field_name.clone(), requires_directive);
                        }
                    }
                }
                ExtendedType::Object(ty) => {
                    for field_name in ty.fields.keys() {
                        let field_definition = schema.type_field(type_name, field_name)?;
                        let field_type = schema.types.get(field_definition.ty.inner_named_type()).ok_or_else(|| {
                            DemandControlError::QueryParseFailure(format!(
                                "Field {} was found in query, but its type is missing from the schema.",
                                field_name
                            ))
                        })?;

                        if let Some(cost_directive) = CostSpecDefinition::cost_directive_from_field(
                            &schema,
                            field_definition,
                            field_type,
                        ) {
                            field_cost_directives.insert(field_name.clone(), cost_directive);
                        }

                        if let Some(list_size_directive) =
                            CostSpecDefinition::list_size_directive_from_field_definition(
                                &schema,
                                field_definition,
                            )
                        {
                            field_list_size_directives
                                .insert(field_name.clone(), list_size_directive);
                        }

                        // TODO: Need to handle renaming for @requires also
                        if let Some(requires_directive) = RequiresDirective::from_field_definition(
                            field_definition,
                            type_name,
                            &schema,
                        )? {
                            field_requires_directives
                                .insert(field_name.clone(), requires_directive);
                        }
                    }
                }
                _ => {
                    // Other types don't have fields
                }
            }
        }

        Ok(Self {
            inner: schema,
            type_field_cost_directives,
            type_field_list_size_directives,
            type_field_requires_directives,
        })
    }

    pub(in crate::plugins::demand_control) fn type_field_cost_directive(
        &self,
        type_name: &str,
        field_name: &str,
    ) -> Option<&CostDirective> {
        self.type_field_cost_directives
            .get(type_name)?
            .get(field_name)
    }

    pub(in crate::plugins::demand_control) fn type_field_list_size_directive(
        &self,
        type_name: &str,
        field_name: &str,
    ) -> Option<&ListSizeDirective> {
        self.type_field_list_size_directives
            .get(type_name)?
            .get(field_name)
    }

    pub(in crate::plugins::demand_control) fn type_field_requires_directive(
        &self,
        type_name: &str,
        field_name: &str,
    ) -> Option<&RequiresDirective> {
        self.type_field_requires_directives
            .get(type_name)?
            .get(field_name)
    }

    pub(in crate::plugins::demand_control) fn argument_cost_directive(
        &self,
        definition: &InputValueDefinition,
        ty: &ExtendedType,
    ) -> Option<CostDirective> {
        CostSpecDefinition::cost_directive_from_argument(&self.inner, definition, ty)
    }
}

impl AsRef<Valid<Schema>> for DemandControlledSchema {
    fn as_ref(&self) -> &Valid<Schema> {
        &self.inner
    }
}

impl Deref for DemandControlledSchema {
    type Target = Schema;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
