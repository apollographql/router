use std::ops::Deref;
use std::sync::Arc;

use ahash::HashMap;
use ahash::HashMapExt;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_federation::link::cost_spec_definition::CostSpecDefinition;
use apollo_federation::link::cost_spec_definition::COST_DIRECTIVE_NAME;
use apollo_federation::link::cost_spec_definition::LIST_SIZE_DIRECTIVE_NAME;
use apollo_federation::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use apollo_federation::link::spec_definition::SpecDefinition;
use apollo_federation::schema::FederationSchema;

use super::directives::CostDirective;
use super::directives::DefinitionListSizeDirective as ListSizeDirective;
use super::directives::RequiresDirective;
use crate::plugins::demand_control::DemandControlError;

pub(crate) struct DemandControlledSchema {
    pub(crate) inner: Arc<Valid<Schema>>,
    cost_directive_name: Option<Name>,
    type_field_cost_directives: HashMap<Name, HashMap<Name, CostDirective>>,
    type_field_list_size_directives: HashMap<Name, HashMap<Name, ListSizeDirective>>,
    type_field_requires_directives: HashMap<Name, HashMap<Name, RequiresDirective>>,
}

impl DemandControlledSchema {
    pub(crate) fn new(schema: Arc<Valid<Schema>>) -> Result<Self, DemandControlError> {
        let fed_schema = FederationSchema::new((*schema).clone().into_inner())?;
        let mut cost_directive_name = None;
        let mut listsize_directive_name = None;

        // Get the actual directive names from the cost spec, and fall back to the federation spec if the cost spec isn't present
        if let Ok(Some(spec)) = CostSpecDefinition::for_schema(&fed_schema) {
            cost_directive_name =
                spec.directive_name_in_schema(&fed_schema, &COST_DIRECTIVE_NAME)?;
            listsize_directive_name =
                spec.directive_name_in_schema(&fed_schema, &LIST_SIZE_DIRECTIVE_NAME)?;
            tracing::debug!(
                "Cost directive names from cost spec: {:?}, {:?}",
                cost_directive_name,
                listsize_directive_name
            );
        } else if let Ok(spec) = get_federation_spec_definition_from_subgraph(&fed_schema) {
            cost_directive_name =
                spec.directive_name_in_schema(&fed_schema, &COST_DIRECTIVE_NAME)?;
            listsize_directive_name =
                spec.directive_name_in_schema(&fed_schema, &LIST_SIZE_DIRECTIVE_NAME)?;
            tracing::debug!(
                "Cost directive names from fed spec: {:?}, {:?}",
                cost_directive_name,
                listsize_directive_name
            );
        }

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

                        if let Some(cost_directive_name) = cost_directive_name.as_ref() {
                            if let Some(cost_directive) =
                                CostDirective::from_field(cost_directive_name, field_definition)
                                    .or(CostDirective::from_type(cost_directive_name, field_type))
                            {
                                field_cost_directives.insert(field_name.clone(), cost_directive);
                            }
                        }

                        if let Some(listsize_directive_name) = listsize_directive_name.as_ref() {
                            if let Some(list_size_directive) =
                                ListSizeDirective::from_field_definition(
                                    listsize_directive_name,
                                    field_definition,
                                )?
                            {
                                field_list_size_directives
                                    .insert(field_name.clone(), list_size_directive);
                            }
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

                        if let Some(cost_directive_name) = cost_directive_name.as_ref() {
                            if let Some(cost_directive) =
                                CostDirective::from_field(cost_directive_name, field_definition)
                                    .or(CostDirective::from_type(cost_directive_name, field_type))
                            {
                                field_cost_directives.insert(field_name.clone(), cost_directive);
                            }
                        }

                        if let Some(listsize_directive_name) = listsize_directive_name.as_ref() {
                            if let Some(list_size_directive) =
                                ListSizeDirective::from_field_definition(
                                    listsize_directive_name,
                                    field_definition,
                                )?
                            {
                                field_list_size_directives
                                    .insert(field_name.clone(), list_size_directive);
                            }
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
            cost_directive_name,
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
        tracing::debug!(
            "Evaluating cost for {} with directive name {:?}\n{:?}\n{:?}",
            definition.name,
            self.cost_directive_name,
            definition.directives,
            ty.directives(),
        );
        if let Some(cost_directive_name) = &self.cost_directive_name {
            CostDirective::from_argument(cost_directive_name, definition)
                .or(CostDirective::from_type(cost_directive_name, ty))
        } else {
            None
        }
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
