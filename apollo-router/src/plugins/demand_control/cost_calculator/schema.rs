use std::ops::Deref;
use std::sync::Arc;

use ahash::HashMap;
use ahash::HashMapExt;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_federation::link::cost_spec_definition::CostDirective;
use apollo_federation::link::cost_spec_definition::CostSpecDefinition;
use apollo_federation::link::cost_spec_definition::ListSizeDirective;
use apollo_federation::schema::ValidFederationSchema;

use super::directives::RequiresDirective;
use crate::plugins::demand_control::DemandControlError;

pub(crate) struct DemandControlledSchema {
    inner: ValidFederationSchema,
    type_field_metadata: HashMap<Name, HashMap<Name, FieldDirectiveMetadata>>,
    type_input_metadata: HashMap<Name, HashMap<Name, InputObjectDirectiveMetadata>>,
}

pub(crate) struct FieldDirectiveMetadata {
    pub(crate) ty: ExtendedType,
    pub(crate) cost_directive: Option<CostDirective>,
    pub(crate) list_size_directive: Option<ListSizeDirective>,
    pub(crate) requires_directive: Option<RequiresDirective>,
    pub(crate) argument_directive_metadata: HashMap<Name, InputObjectDirectiveMetadata>,
}

pub(crate) struct InputObjectDirectiveMetadata {
    pub(crate) ty: ExtendedType,
    pub(crate) cost_directive: Option<CostDirective>,
}

impl DemandControlledSchema {
    pub(crate) fn new(schema: Arc<Valid<Schema>>) -> Result<Self, DemandControlError> {
        let fed_schema = ValidFederationSchema::new((*schema).clone())?;
        let mut type_field_metadata: HashMap<Name, HashMap<Name, FieldDirectiveMetadata>> =
            HashMap::new();
        let mut type_input_metadata: HashMap<Name, HashMap<Name, InputObjectDirectiveMetadata>> =
            HashMap::new();

        for (type_name, type_) in &schema.types {
            match type_ {
                ExtendedType::Interface(ty) => {
                    let type_metadata = type_field_metadata.entry(type_name.clone()).or_default();
                    for field_name in ty.fields.keys() {
                        let field_definition = schema.type_field(type_name, field_name)?;
                        let field_type = schema.types.get(field_definition.ty.inner_named_type()).ok_or_else(|| {
                            DemandControlError::QueryParseFailure(format!(
                                "Field {} was found in query, but its type is missing from the schema.",
                                field_name
                            ))
                        })?;

                        let field_metadata = type_metadata
                            .entry(field_name.clone())
                            .or_insert_with(|| FieldDirectiveMetadata {
                                ty: field_type.clone(),
                                cost_directive: None,
                                list_size_directive: None,
                                requires_directive: None,
                                argument_directive_metadata: HashMap::new(),
                            });
                        field_metadata.cost_directive =
                            CostSpecDefinition::cost_directive_from_field(
                                &fed_schema,
                                field_definition,
                                field_type,
                            )?;
                        field_metadata.list_size_directive =
                            CostSpecDefinition::list_size_directive_from_field_definition(
                                &fed_schema,
                                field_definition,
                            )?;
                        field_metadata.requires_directive =
                            RequiresDirective::from_field_definition(
                                field_definition,
                                type_name,
                                &schema,
                            )?;

                        for argument_definition in &field_definition.arguments {
                            let argument_ty = schema
                                .types
                                .get(argument_definition.ty.inner_named_type())
                                .ok_or_else(|| {
                                    DemandControlError::QueryParseFailure(format!(
                                        "Argument {}({}:) has type {}, but that type was not found in the schema",
                                        field_definition.name,
                                        argument_definition.name,
                                        argument_definition.ty.inner_named_type()
                                    ))
                                })?;
                            field_metadata.argument_directive_metadata.insert(
                                argument_definition.name.clone(),
                                InputObjectDirectiveMetadata {
                                    ty: argument_ty.clone(),
                                    cost_directive:
                                        CostSpecDefinition::cost_directive_from_argument(
                                            &fed_schema,
                                            argument_definition,
                                            argument_ty,
                                        )?,
                                },
                            );
                        }
                    }
                }
                ExtendedType::Object(ty) => {
                    let type_metadata = type_field_metadata.entry(type_name.clone()).or_default();
                    for field_name in ty.fields.keys() {
                        let field_definition = schema.type_field(type_name, field_name)?;
                        let field_type = schema.types.get(field_definition.ty.inner_named_type()).ok_or_else(|| {
                            DemandControlError::QueryParseFailure(format!(
                                "Field {} was found in query, but its type is missing from the schema.",
                                field_name
                            ))
                        })?;

                        let field_metadata = type_metadata
                            .entry(field_name.clone())
                            .or_insert_with(|| FieldDirectiveMetadata {
                                ty: field_type.clone(),
                                cost_directive: None,
                                list_size_directive: None,
                                requires_directive: None,
                                argument_directive_metadata: HashMap::new(),
                            });
                        field_metadata.cost_directive =
                            CostSpecDefinition::cost_directive_from_field(
                                &fed_schema,
                                field_definition,
                                field_type,
                            )?;
                        field_metadata.list_size_directive =
                            CostSpecDefinition::list_size_directive_from_field_definition(
                                &fed_schema,
                                field_definition,
                            )?;
                        field_metadata.requires_directive =
                            RequiresDirective::from_field_definition(
                                field_definition,
                                type_name,
                                &schema,
                            )?;

                        for argument_definition in &field_definition.arguments {
                            let argument_ty = schema
                                .types
                                .get(argument_definition.ty.inner_named_type())
                                .ok_or_else(|| {
                                    DemandControlError::QueryParseFailure(format!(
                                        "Argument {}({}:) has type {}, but that type was not found in the schema",
                                        field_definition.name,
                                        argument_definition.name,
                                        argument_definition.ty.inner_named_type()
                                    ))
                                })?;
                            field_metadata.argument_directive_metadata.insert(
                                argument_definition.name.clone(),
                                InputObjectDirectiveMetadata {
                                    ty: argument_ty.clone(),
                                    cost_directive:
                                        CostSpecDefinition::cost_directive_from_argument(
                                            &fed_schema,
                                            argument_definition,
                                            argument_ty,
                                        )?,
                                },
                            );
                        }
                    }
                }
                ExtendedType::InputObject(ty) => {
                    let type_metadata = type_input_metadata.entry(type_name.clone()).or_default();
                    for (field_name, field_definition) in &ty.fields {
                        let field_type = schema.types.get(field_definition.ty.inner_named_type()).ok_or_else(|| {
                            DemandControlError::QueryParseFailure(format!(
                                "Field {} was found in query, but its type is missing from the schema.",
                                field_name
                            ))
                        })?;
                        type_metadata.insert(
                            field_name.clone(),
                            InputObjectDirectiveMetadata {
                                ty: field_type.clone(),
                                cost_directive: CostSpecDefinition::cost_directive_from_argument(
                                    &fed_schema,
                                    &field_definition,
                                    field_type,
                                )?,
                            },
                        );
                    }
                }
                _ => {
                    // Other types don't have fields
                }
            }
        }

        Ok(Self {
            inner: fed_schema,
            type_field_metadata,
            type_input_metadata,
        })
    }

    pub(in crate::plugins::demand_control) fn type_field_metadata(
        &self,
        type_name: &str,
        field_name: &str,
    ) -> Option<&FieldDirectiveMetadata> {
        self.type_field_metadata.get(type_name)?.get(field_name)
    }

    pub(in crate::plugins::demand_control) fn type_input_metadata(
        &self,
        type_name: &str,
        field_name: &str,
    ) -> Option<&InputObjectDirectiveMetadata> {
        self.type_input_metadata.get(type_name)?.get(field_name)
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
