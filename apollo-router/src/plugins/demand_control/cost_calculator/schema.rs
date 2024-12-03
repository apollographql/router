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
use apollo_federation::schema::ValidFederationSchema;
use rhai::Variant;

use super::directives::RequiresDirective;
use crate::plugins::demand_control::DemandControlError;

pub(crate) struct DemandControlledSchema {
    inner: ValidFederationSchema,
    type_field_metadata: HashMap<Name, HashMap<Name, FieldDirectiveMetadata>>,
    type_input_metadata: HashMap<Name, HashMap<Name, InputObjectDirectiveMetadata>>,
}

pub(crate) struct FieldDirectiveMetadata {
    pub(crate) name: Name,
    pub(crate) ty: ExtendedType,
    pub(crate) cost_directive: Option<CostDirective>,
    pub(crate) list_size_directive: Option<ListSizeDirective>,
    pub(crate) requires_directive: Option<RequiresDirective>,
    argument_directive_metadata: HashMap<Name, InputObjectDirectiveMetadata>,
}

impl FieldDirectiveMetadata {
    pub(crate) fn argument_metadata(
        &self,
        arg_name: &str,
    ) -> Result<&InputObjectDirectiveMetadata, DemandControlError> {
        self.argument_directive_metadata
            .get(arg_name)
            .ok_or_else(|| DemandControlError::ArgumentLookupError {
                field_name: self.name.to_string(),
                arg_name: arg_name.to_string(),
            })
    }
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
                        Self::record_field_metadata(&fed_schema, type_, field_name, type_metadata)?;
                    }
                }
                ExtendedType::Object(ty) => {
                    let type_metadata = type_field_metadata.entry(type_name.clone()).or_default();
                    for field_name in ty.fields.keys() {
                        Self::record_field_metadata(&fed_schema, type_, field_name, type_metadata)?;
                    }
                }
                ExtendedType::InputObject(ty) => {
                    let type_metadata = type_input_metadata.entry(type_name.clone()).or_default();
                    for (field_name, field_definition) in &ty.fields {
                        Self::record_input_object_metadata(
                            &fed_schema,
                            field_name,
                            field_definition,
                            type_metadata,
                        )?;
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

    fn record_field_metadata(
        schema: &ValidFederationSchema,
        ty: &ExtendedType,
        field_name: &Name,
        type_metadata: &mut HashMap<Name, FieldDirectiveMetadata>,
    ) -> Result<(), DemandControlError> {
        let field_definition = schema.schema().type_field(ty.type_name(), field_name)?;
        let field_type = schema
            .schema()
            .types
            .get(field_definition.ty.inner_named_type())
            .ok_or_else(|| DemandControlError::FieldLookupError {
                type_name: ty.type_name().to_string(),
                field_name: field_name.to_string(),
            })?;

        let field_metadata =
            type_metadata
                .entry(field_name.clone())
                .or_insert_with(|| FieldDirectiveMetadata {
                    name: field_name.clone(),
                    ty: field_type.clone(),
                    cost_directive: None,
                    list_size_directive: None,
                    requires_directive: None,
                    argument_directive_metadata: HashMap::new(),
                });
        field_metadata.cost_directive =
            CostSpecDefinition::cost_directive_from_field(schema, field_definition, field_type)?;
        field_metadata.list_size_directive =
            CostSpecDefinition::list_size_directive_from_field_definition(
                schema,
                field_definition,
            )?;
        field_metadata.requires_directive =
            RequiresDirective::from_field_definition(field_definition, ty.name(), schema.schema())?;

        for argument_definition in &field_definition.arguments {
            Self::record_input_object_metadata(
                schema,
                field_name,
                argument_definition,
                &mut field_metadata.argument_directive_metadata,
            )?;
        }

        Ok(())
    }

    fn record_input_object_metadata(
        schema: &ValidFederationSchema,
        field_name: &Name,
        argument_definition: &InputValueDefinition,
        field_metadata: &mut HashMap<Name, InputObjectDirectiveMetadata>,
    ) -> Result<(), DemandControlError> {
        let argument_ty = schema
            .schema()
            .types
            .get(argument_definition.ty.inner_named_type())
            .ok_or_else(|| DemandControlError::ArgumentLookupError {
                field_name: field_name.to_string(),
                arg_name: argument_definition.name.to_string(),
            })?;
        field_metadata.insert(
            argument_definition.name.clone(),
            InputObjectDirectiveMetadata {
                ty: argument_ty.clone(),
                cost_directive: CostSpecDefinition::cost_directive_from_argument(
                    schema,
                    argument_definition,
                    argument_ty,
                )?,
            },
        );

        Ok(())
    }

    pub(in crate::plugins::demand_control) fn type_field_metadata(
        &self,
        type_name: &str,
        field_name: &str,
    ) -> Result<&FieldDirectiveMetadata, DemandControlError> {
        self.type_field_metadata
            .get(type_name)
            .and_then(|m| m.get(field_name))
            .ok_or_else(|| DemandControlError::FieldLookupError {
                type_name: type_name.to_string(),
                field_name: field_name.to_string(),
            })
    }

    pub(in crate::plugins::demand_control) fn type_input_metadata(
        &self,
        type_name: &str,
        field_name: &str,
    ) -> Result<&InputObjectDirectiveMetadata, DemandControlError> {
        self.type_input_metadata
            .get(type_name)
            .and_then(|m| m.get(field_name))
            .ok_or_else(|| DemandControlError::FieldLookupError {
                type_name: type_name.to_string(),
                field_name: field_name.to_string(),
            })
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
