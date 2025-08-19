use std::collections::HashMap;
use std::collections::HashSet;

use apollo_compiler::Schema;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;

use crate::error::FederationError;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::schema::FederationSchema;
use crate::schema::field_set::collect_target_fields_from_field_set;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::InterfaceFieldDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::validators::from_context::parse_context;

fn unwrap_schema(fed_schema: &FederationSchema) -> &Valid<Schema> {
    // Okay to assume valid because `fed_schema` is known to be valid.
    Valid::assume_valid_ref(fed_schema.schema())
}

// PORT_NOTE: The JS codebase called this `FederationMetadata`, but this naming didn't make it
// apparent that this was just subgraph schema metadata, so we've renamed it accordingly.
#[derive(Debug, Clone)]
pub(crate) struct SubgraphMetadata {
    federation_spec_definition: &'static FederationSpecDefinition,
    external_metadata: ExternalMetadata,
    context_fields: IndexSet<FieldDefinitionPosition>,
    interface_constraint_fields: IndexSet<FieldDefinitionPosition>,
    key_fields: IndexSet<FieldDefinitionPosition>,
    provided_fields: IndexSet<FieldDefinitionPosition>,
    required_fields: IndexSet<FieldDefinitionPosition>,
    shareable_fields: IndexSet<FieldDefinitionPosition>,
}

#[allow(dead_code)]
impl SubgraphMetadata {
    pub(super) fn new(
        schema: &FederationSchema,
        federation_spec_definition: &'static FederationSpecDefinition,
    ) -> Result<Self, FederationError> {
        let external_metadata = ExternalMetadata::new(schema, federation_spec_definition)?;
        let context_fields = Self::collect_fields_used_by_context_directive(schema)?;
        let interface_constraint_fields =
            Self::collect_fields_used_to_satisfy_interface_constraints(schema)?;
        let key_fields = Self::collect_key_fields(schema)?;
        let provided_fields = Self::collect_provided_fields(schema)?;
        let required_fields = Self::collect_required_fields(schema)?;
        let shareable_fields = if federation_spec_definition.is_fed1() {
            // `@shareable` is not used in Fed 1 schemas.
            Default::default()
        } else {
            Self::collect_shareable_fields(schema, federation_spec_definition)?
        };

        Ok(Self {
            federation_spec_definition,
            external_metadata,
            context_fields,
            interface_constraint_fields,
            key_fields,
            provided_fields,
            required_fields,
            shareable_fields,
        })
    }

    pub(crate) fn federation_spec_definition(&self) -> &'static FederationSpecDefinition {
        self.federation_spec_definition
    }

    pub(crate) fn external_metadata(&self) -> &ExternalMetadata {
        &self.external_metadata
    }

    pub(crate) fn is_fed_2_schema(&self) -> bool {
        self.federation_spec_definition()
            .version()
            .satisfies(&Version { major: 2, minor: 0 })
    }

    pub(crate) fn is_field_external(&self, field: &FieldDefinitionPosition) -> bool {
        self.external_metadata().is_external(field)
    }

    pub(crate) fn is_field_external_in_implementer(&self, field: &FieldDefinitionPosition) -> bool {
        self.external_metadata().is_external_in_implementer(field)
    }

    pub(crate) fn is_field_fake_external(&self, field: &FieldDefinitionPosition) -> bool {
        self.external_metadata().is_fake_external(field)
    }

    pub(crate) fn is_field_fully_external(&self, field: &FieldDefinitionPosition) -> bool {
        self.is_field_external(field) && self.provided_fields.contains(field)
    }

    pub(crate) fn is_field_partially_external(&self, field: &FieldDefinitionPosition) -> bool {
        self.is_field_external(field) && !self.provided_fields.contains(field)
    }

    pub(crate) fn is_field_shareable(&self, field: &FieldDefinitionPosition) -> bool {
        self.key_fields.contains(field)
            || self.shareable_fields.contains(field)
            // Fed2 schemas reject provides on non-external field, but fed1 doesn't (at least not always).
            // We call this on fed1 schema upgrader. So let's make sure we ignore non-external fields.
            || (self.provided_fields.contains(field) && self.is_field_external(field))
    }

    pub(crate) fn is_field_used(&self, field: &FieldDefinitionPosition) -> bool {
        self.context_fields.contains(field)
            || self.interface_constraint_fields.contains(field)
            || self.key_fields.contains(field)
            || self.provided_fields.contains(field)
            || self.required_fields.contains(field)
    }

    pub(crate) fn selection_selects_any_external_field(&self, selection: &SelectionSet) -> bool {
        self.external_metadata()
            .selects_any_external_field(selection)
    }

    fn collect_key_fields(
        schema: &FederationSchema,
    ) -> Result<IndexSet<FieldDefinitionPosition>, FederationError> {
        let mut key_fields = IndexSet::default();
        let Ok(applications) = schema.key_directive_applications() else {
            return Ok(Default::default());
        };
        for key_directive in applications.into_iter().filter_map(|res| res.ok()) {
            key_fields.extend(collect_target_fields_from_field_set(
                unwrap_schema(schema),
                key_directive.target.type_name().clone(),
                key_directive.arguments.fields,
                false,
            )?);
        }
        Ok(key_fields)
    }

    fn collect_provided_fields(
        schema: &FederationSchema,
    ) -> Result<IndexSet<FieldDefinitionPosition>, FederationError> {
        let mut provided_fields = IndexSet::default();
        let Ok(applications) = schema.provides_directive_applications() else {
            return Ok(Default::default());
        };
        for provides_directive in applications.into_iter().filter_map(|res| res.ok()) {
            provided_fields.extend(collect_target_fields_from_field_set(
                unwrap_schema(schema),
                provides_directive.target_return_type.clone(),
                provides_directive.arguments.fields,
                false,
            )?);
        }
        Ok(provided_fields)
    }

    fn collect_required_fields(
        schema: &FederationSchema,
    ) -> Result<IndexSet<FieldDefinitionPosition>, FederationError> {
        let mut required_fields = IndexSet::default();
        let Ok(applications) = schema.requires_directive_applications() else {
            return Ok(Default::default());
        };
        for requires_directive in applications.into_iter().filter_map(|d| d.ok()) {
            required_fields.extend(collect_target_fields_from_field_set(
                unwrap_schema(schema),
                requires_directive.target.type_name().clone(),
                requires_directive.arguments.fields,
                false,
            )?);
        }
        Ok(required_fields)
    }

    fn collect_shareable_fields(
        schema: &FederationSchema,
        federation_spec_definition: &'static FederationSpecDefinition,
    ) -> Result<IndexSet<FieldDefinitionPosition>, FederationError> {
        let mut shareable_fields = IndexSet::default();
        // PORT_NOTE: The comment below is from the JS code. It doesn't seem to apply to the Rust code.
        // @shareable is only available on fed2 schemas, but the schema upgrader call this on fed1 schemas as a shortcut to
        // identify key fields (because if we know nothing is marked @shareable, then the only fields that are shareable
        // by default are key fields).
        let Some(shareable_directive_name) =
            federation_spec_definition.shareable_directive_name_in_schema(schema)?
        else {
            return Ok(shareable_fields);
        };
        let shareable_directive_referencers = schema
            .referencers
            .get_directive(&shareable_directive_name)?;

        // Fields of shareable object types are shareable
        for object_type_position in &shareable_directive_referencers.object_types {
            shareable_fields.extend(
                object_type_position
                    .fields(schema.schema())?
                    .map(FieldDefinitionPosition::Object),
            );
        }

        // Fields with @shareable directly applied are shareable
        shareable_fields.extend(
            shareable_directive_referencers
                .object_fields
                .iter()
                .cloned()
                .map(FieldDefinitionPosition::Object),
        );

        Ok(shareable_fields)
    }

    fn collect_fields_used_by_context_directive(
        schema: &FederationSchema,
    ) -> Result<IndexSet<FieldDefinitionPosition>, FederationError> {
        let Ok(context_directive_applications) = schema.context_directive_applications() else {
            return Ok(Default::default());
        };
        let Ok(from_context_directive_applications) = schema.from_context_directive_applications()
        else {
            return Ok(Default::default());
        };

        let mut used_context_fields = IndexSet::default();
        let mut entry_points: HashMap<String, HashSet<CompositeTypeDefinitionPosition>> =
            HashMap::new();

        for context_directive in context_directive_applications
            .into_iter()
            .filter_map(|d| d.ok())
        {
            if !entry_points.contains_key(context_directive.arguments.name) {
                entry_points.insert(context_directive.arguments.name.to_string(), HashSet::new());
            }
            entry_points
                .get_mut(context_directive.arguments.name)
                .expect("was just inserted")
                .insert(context_directive.target);
        }
        for from_context_directive in from_context_directive_applications
            .into_iter()
            .filter_map(|d| d.ok())
        {
            let (Some(context), Some(selection)) =
                parse_context(from_context_directive.arguments.field)
            else {
                continue;
            };
            if let Some(entry_point) = entry_points.get(context.as_str()) {
                for context_type in entry_point {
                    used_context_fields.extend(collect_target_fields_from_field_set(
                        unwrap_schema(schema),
                        context_type.type_name().clone(),
                        selection.as_str(),
                        false,
                    )?);
                }
            }
        }
        Ok(used_context_fields)
    }

    fn collect_fields_used_to_satisfy_interface_constraints(
        schema: &FederationSchema,
    ) -> Result<IndexSet<FieldDefinitionPosition>, FederationError> {
        let mut interface_constraint_fields = IndexSet::default();
        for ty in schema.schema().types.values() {
            if let ExtendedType::Interface(itf) = ty {
                let possible_runtime_types: Vec<_> = schema
                    .schema()
                    .implementers_map()
                    .get(&itf.name)
                    .map_or(&Default::default(), |impls| &impls.objects)
                    .iter()
                    .filter_map(|ty| schema.schema().get_object(ty))
                    .collect();

                for field_name in itf.fields.keys() {
                    for object_type in &possible_runtime_types {
                        interface_constraint_fields.insert(FieldDefinitionPosition::Object(
                            ObjectFieldDefinitionPosition {
                                type_name: object_type.name.clone(),
                                field_name: field_name.clone(),
                            },
                        ));
                    }
                }
            }
        }

        Ok(interface_constraint_fields)
    }
}

// PORT_NOTE: The JS codebase called this `ExternalTester`, but this naming didn't make it
// apparent that this was just @external-related subgraph metadata, so we've renamed it accordingly.
// Also note the field "externalFieldsOnType" was renamed to "fields_on_external_types", as it's
// more accurate.
#[derive(Debug, Clone)]
pub(crate) struct ExternalMetadata {
    /// All fields with an `@external` directive.
    external_fields: IndexSet<FieldDefinitionPosition>,
    /// Fields with an `@external` directive that can't actually be external due to also being
    /// referenced in a `@key` directive.
    fake_external_fields: IndexSet<FieldDefinitionPosition>,
    /// Fields that are external because their parent type has an `@external` directive.
    fields_on_external_types: IndexSet<FieldDefinitionPosition>,
    /// Fields which are not necessarily external on their source interface but have an implementation
    /// which does mark that field as external.
    fields_with_external_implementation: IndexSet<FieldDefinitionPosition>,
}

impl ExternalMetadata {
    fn new(
        schema: &FederationSchema,
        federation_spec_definition: &'static FederationSpecDefinition,
    ) -> Result<Self, FederationError> {
        let external_fields = Self::collect_external_fields(federation_spec_definition, schema)?;
        let fake_external_fields =
            Self::collect_fake_externals(federation_spec_definition, schema)?;
        // We do not collect @external on types for Fed 1 schemas since those will be discarded by
        // the schema upgrader. The schema upgrader, through calls to `is_external()`, relies on the
        // populated `fields_on_external_types` set to inform when @shareable should be
        // automatically added. In the Fed 1 case, if the set is populated then @shareable won't be
        // added in places where it should be.
        let is_fed2 = federation_spec_definition
            .version()
            .satisfies(&Version { major: 2, minor: 0 });
        let fields_on_external_types = if is_fed2 {
            Self::collect_fields_on_external_types(federation_spec_definition, schema)?
        } else {
            Default::default()
        };

        // We want to be able to check if an interface field has an implementation which marks it as external.
        // We take the external fields we already collected and find possible interface candidates for object
        // types. Then, we filter down to those candidates which actually have the field we're looking at.
        let fields_with_external_implementation = external_fields
            .iter()
            .flat_map(|external_field| {
                let Ok(ExtendedType::Object(ty)) = external_field.parent().get(schema.schema())
                else {
                    return vec![];
                };
                ty.implements_interfaces
                    .iter()
                    .map(|itf| {
                        FieldDefinitionPosition::Interface(InterfaceFieldDefinitionPosition {
                            type_name: itf.name.clone(),
                            field_name: external_field.field_name().clone(),
                        })
                    })
                    .filter(|candidate_field| candidate_field.try_get(schema.schema()).is_some())
                    .collect()
            })
            .collect();

        Ok(Self {
            external_fields,
            fake_external_fields,
            fields_on_external_types,
            fields_with_external_implementation,
        })
    }

    fn collect_external_fields(
        federation_spec_definition: &'static FederationSpecDefinition,
        schema: &FederationSchema,
    ) -> Result<IndexSet<FieldDefinitionPosition>, FederationError> {
        let Some(external_directive_name) =
            federation_spec_definition.external_directive_name_in_schema(schema)?
        else {
            return Ok(Default::default());
        };

        let external_directive_referencers =
            schema.referencers.get_directive(&external_directive_name)?;

        let mut external_fields = IndexSet::default();

        external_fields.extend(
            external_directive_referencers
                .object_fields
                .iter()
                .map(|field| field.clone().into()),
        );

        external_fields.extend(
            external_directive_referencers
                .interface_fields
                .iter()
                .map(|field| field.clone().into()),
        );

        Ok(external_fields)
    }

    fn collect_fake_externals(
        federation_spec_definition: &'static FederationSpecDefinition,
        schema: &FederationSchema,
    ) -> Result<IndexSet<FieldDefinitionPosition>, FederationError> {
        let mut fake_external_fields = IndexSet::default();
        let (Ok(extends_directive_definition), Ok(key_directive_applications)) = (
            federation_spec_definition.extends_directive_definition(schema),
            schema.key_directive_applications(),
        ) else {
            return Ok(Default::default());
        };
        for key_directive in key_directive_applications
            .into_iter()
            .filter_map(|k| k.ok())
        {
            let has_extends_directive = key_directive
                .sibling_directives
                .has(&extends_directive_definition.name);
            // PORT_NOTE: The JS codebase treats the "extend" GraphQL keyword as applying to
            // only the extension it's on, while it treats the "@extends" directive as applying
            // to all definitions/extensions in the subgraph. We accordingly do the same.
            if has_extends_directive
                || key_directive
                    .schema_directive
                    .origin
                    .extension_id()
                    .is_some()
            {
                fake_external_fields.extend(collect_target_fields_from_field_set(
                    unwrap_schema(schema),
                    key_directive.target.type_name().clone(),
                    key_directive.arguments.fields,
                    false,
                )?);
            }
        }
        Ok(fake_external_fields)
    }

    fn collect_fields_on_external_types(
        federation_spec_definition: &'static FederationSpecDefinition,
        schema: &FederationSchema,
    ) -> Result<IndexSet<FieldDefinitionPosition>, FederationError> {
        let Some(external_directive_name) =
            federation_spec_definition.external_directive_name_in_schema(schema)?
        else {
            return Ok(Default::default());
        };

        let external_directive_referencers =
            schema.referencers.get_directive(&external_directive_name)?;

        let mut fields_on_external_types = IndexSet::default();
        for object_type_position in &external_directive_referencers.object_types {
            let object_type = object_type_position.get(schema.schema())?;
            // PORT_NOTE: The JS codebase does not differentiate fields at a definition/extension
            // level here, and we accordingly do the same. I.e., if a type is marked @external for
            // one definition/extension in a subgraph, then it is considered to be marked @external
            // for all definitions/extensions in that subgraph.
            for field_name in object_type.fields.keys() {
                fields_on_external_types
                    .insert(object_type_position.field(field_name.clone()).into());
            }
        }
        Ok(fields_on_external_types)
    }

    pub(crate) fn is_external(&self, field_definition_position: &FieldDefinitionPosition) -> bool {
        (self.external_fields.contains(field_definition_position)
            || self
                .fields_on_external_types
                .contains(field_definition_position))
            && !self.is_fake_external(field_definition_position)
    }

    pub(crate) fn is_external_in_implementer(
        &self,
        field_definition_position: &FieldDefinitionPosition,
    ) -> bool {
        self.fields_with_external_implementation
            .contains(field_definition_position)
    }

    pub(crate) fn is_fake_external(
        &self,
        field_definition_position: &FieldDefinitionPosition,
    ) -> bool {
        self.fake_external_fields
            .contains(field_definition_position)
    }

    pub(crate) fn selects_any_external_field(&self, selection_set: &SelectionSet) -> bool {
        for selection in selection_set.selections.values() {
            if let Selection::Field(field_selection) = selection
                && self.is_external(&field_selection.field.field_position)
            {
                return true;
            }
            if let Some(selection_set) = selection.selection_set()
                && self.selects_any_external_field(selection_set)
            {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Name;

    use crate::schema::FederationSchema;
    use crate::schema::position::FieldDefinitionPosition;
    use crate::schema::position::ObjectFieldDefinitionPosition;

    #[test]
    fn subgraph_metadata_is_field_shareable() {
        let schema_str = include_str!("fixtures/shareable_fields.graphqls");
        let schema = apollo_compiler::Schema::parse(schema_str, "shareable_fields.graphqls")
            .expect("valid schema");
        let fed_schema = FederationSchema::new(schema)
            .expect("federation schema")
            .validate_or_return_self()
            .map_err(|(_, diagnostics)| diagnostics)
            .expect("valid federation schema");
        let meta = fed_schema.subgraph_metadata().expect("has metadata");

        // Fields on @shareable object types are shareable
        assert!(meta.is_field_shareable(&field("O1", "a")));
        assert!(meta.is_field_shareable(&field("O1", "b")));

        // Fields directly marked with @shareable are shareable
        assert!(meta.is_field_shareable(&field("O2", "d")));

        // Fields marked as @external and provided by some path in the graph are shareable
        assert!(meta.is_field_shareable(&field("O3", "externalField")));

        // Remaining fields are not shareable
        assert!(!meta.is_field_shareable(&field("O2", "c")));
        assert!(!meta.is_field_shareable(&field("O3", "c")));
        assert!(!meta.is_field_shareable(&field("O3", "externalFieldNeverProvided")));
    }

    #[test]
    fn subgraph_metadata_is_field_used() {
        let schema_str = include_str!("fixtures/used_fields.graphqls");
        let schema = apollo_compiler::Schema::parse(schema_str, "used_fields.graphqls")
            .expect("valid schema");
        let fed_schema = FederationSchema::new(schema)
            .expect("federation schema")
            .validate_or_return_self()
            .map_err(|(_, diagnostics)| diagnostics)
            .expect("valid federation schema");
        let meta = fed_schema.subgraph_metadata().expect("has metadata");

        // Fields that can satisfy interface constraints are used
        assert!(meta.is_field_used(&field("O1", "a")));

        // Fields required by @requires are used
        assert!(meta.is_field_used(&field("O2", "isRequired")));
        assert!(meta.is_field_used(&field("O2", "isAlsoRequired")));

        // Fields that are part of a @key are used
        assert!(meta.is_field_used(&field("O3", "keyField1")));
        assert!(meta.is_field_used(&field("O3", "subKey")));
        assert!(meta.is_field_used(&field("O3SubKey", "keyField2")));

        // Fields that are @external and provided by some path in the graph are used
        assert!(meta.is_field_used(&field("O4", "externalField")));

        // Fields pulled from @context are used
        assert!(meta.is_field_used(&field("O5Context", "usedInContext")));

        // Remaining fields are not considered used
        assert!(!meta.is_field_used(&field("O1", "b")));
        assert!(!meta.is_field_used(&field("O2", "hasRequirement")));
        assert!(!meta.is_field_used(&field("O3", "nonKeyField")));
        assert!(!meta.is_field_used(&field("O4", "c")));
        assert!(!meta.is_field_used(&field("O4", "externalFieldNeverProvided")));
        assert!(!meta.is_field_used(&field("O5Context", "notUsedInContext")));
    }

    fn field(type_name: &str, field_name: &str) -> FieldDefinitionPosition {
        FieldDefinitionPosition::Object(ObjectFieldDefinitionPosition {
            type_name: Name::new_unchecked(type_name),
            field_name: Name::new_unchecked(field_name),
        })
    }
}
