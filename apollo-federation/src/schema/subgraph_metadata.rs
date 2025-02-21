use std::collections::HashMap;
use std::collections::HashSet;

use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::Name;
use apollo_compiler::Schema;

use crate::error::FederationError;
use crate::link::context_spec_definition::parse_context;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::schema::field_set::collect_target_fields_from_field_set;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceTypeDefinitionPosition;
use crate::schema::FederationSchema;

use super::position::CompositeTypeDefinitionPosition;
use super::position::ObjectFieldDefinitionPosition;

fn unwrap_schema(fed_schema: &Valid<FederationSchema>) -> &Valid<Schema> {
    // Okay to assume valid because `fed_schema` is known to be valid.
    Valid::assume_valid_ref(fed_schema.schema())
}

// PORT_NOTE: The JS codebase called this `FederationMetadata`, but this naming didn't make it
// apparent that this was just subgraph schema metadata, so we've renamed it accordingly.
#[derive(Debug, Clone)]
pub(crate) struct SubgraphMetadata {
    federation_spec_definition: &'static FederationSpecDefinition,
    external_metadata: ExternalMetadata,
    shareable_fields: IndexSet<FieldDefinitionPosition>,
    used_fields: IndexSet<FieldDefinitionPosition>,
}

impl SubgraphMetadata {
    pub(super) fn new(
        schema: &Valid<FederationSchema>,
        federation_spec_definition: &'static FederationSpecDefinition,
    ) -> Result<Self, FederationError> {
        let external_metadata = ExternalMetadata::new(schema, federation_spec_definition)?;
        let shareable_fields =
            Self::collect_shareable_fields(schema, federation_spec_definition, &external_metadata)?;
        let used_fields = Self::collect_used_fields(schema, federation_spec_definition)?;
        Ok(Self {
            federation_spec_definition,
            external_metadata,
            shareable_fields,
            used_fields,
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

    pub(crate) fn is_field_fake_external(&self, field: &FieldDefinitionPosition) -> bool {
        self.external_metadata().is_fake_external(field)
    }

    pub(crate) fn is_field_fully_external(&self, field: &FieldDefinitionPosition) -> bool {
        self.external_metadata().is_fully_external(field)
    }

    pub(crate) fn is_field_partially_external(&self, field: &FieldDefinitionPosition) -> bool {
        self.external_metadata().is_partially_external(field)
    }

    pub(crate) fn is_field_shareable(&self, field: &FieldDefinitionPosition) -> bool {
        self.shareable_fields.contains(field)
    }

    pub(crate) fn is_field_used(&self, field: &FieldDefinitionPosition) -> bool {
        self.used_fields.contains(field)
    }

    pub(crate) fn is_interface_object_type(&self, type_: Name) -> bool {
        todo!()
    }

    pub(crate) fn selection_selects_any_external_field(&self, selection: &SelectionSet) -> bool {
        self.external_metadata()
            .selects_any_external_field(selection)
    }

    fn collect_shareable_fields(
        schema: &Valid<FederationSchema>,
        federation_spec_definition: &'static FederationSpecDefinition,
        external_metadata: &ExternalMetadata,
    ) -> Result<IndexSet<FieldDefinitionPosition>, FederationError> {
        let mut shareable_fields = IndexSet::default();
        shareable_fields.extend(Self::collect_fields_used_by_key_directive(
            schema,
            federation_spec_definition,
        )?);

        shareable_fields.extend(
            Self::collect_fields_used_by_provides_directive(schema, federation_spec_definition)?
                .iter()
                .cloned()
                .filter(|f| external_metadata.is_external(f)),
        );

        // TODO: No shareable directive is used as a fed1/fed2 indicator in JS, so this case should probably just return the
        // fields collected by the @key directive check
        let shareable_directive_definition =
            federation_spec_definition.shareable_directive_definition(schema)?;
        let shareable_directive_referencers = schema
            .referencers
            .get_directive(&shareable_directive_definition.name)?;

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

    fn collect_used_fields(
        schema: &Valid<FederationSchema>,
        federation_spec_definition: &'static FederationSpecDefinition,
    ) -> Result<IndexSet<FieldDefinitionPosition>, FederationError> {
        let mut used_fields = IndexSet::default();
        used_fields.extend(Self::collect_fields_used_by_key_directive(
            schema,
            federation_spec_definition,
        )?);
        used_fields.extend(Self::collect_fields_used_by_requires_directive(
            schema,
            federation_spec_definition,
        )?);
        used_fields.extend(Self::collect_fields_used_by_provides_directive(
            schema,
            federation_spec_definition,
        )?);
        used_fields.extend(Self::collect_fields_used_by_context_directive(
            schema,
            federation_spec_definition,
        )?);
        Self::collect_fields_used_to_satisfy_interface_constraints(schema, &mut used_fields)?;

        Ok(used_fields)
    }

    fn collect_fields_used_by_key_directive(
        schema: &Valid<FederationSchema>,
        federation_spec_definition: &'static FederationSpecDefinition,
    ) -> Result<IndexSet<FieldDefinitionPosition>, FederationError> {
        let key_directive_definition =
            federation_spec_definition.key_directive_definition(schema)?;
        let key_directive_referencers = schema
            .referencers
            .get_directive(&key_directive_definition.name)?;
        let mut key_type_positions: Vec<ObjectOrInterfaceTypeDefinitionPosition> = vec![];
        for object_type_position in &key_directive_referencers.object_types {
            key_type_positions.push(object_type_position.clone().into());
        }
        for interface_type_position in &key_directive_referencers.interface_types {
            key_type_positions.push(interface_type_position.clone().into());
        }

        let mut key_fields = IndexSet::default();
        for type_position in key_type_positions {
            let directives = match &type_position {
                ObjectOrInterfaceTypeDefinitionPosition::Object(pos) => {
                    &pos.get(schema.schema())?.directives
                }
                ObjectOrInterfaceTypeDefinitionPosition::Interface(pos) => {
                    &pos.get(schema.schema())?.directives
                }
            };
            for key_directive_application in directives.get_all(&key_directive_definition.name) {
                let key_directive_arguments = federation_spec_definition
                    .key_directive_arguments(key_directive_application)?;
                key_fields.extend(collect_target_fields_from_field_set(
                    unwrap_schema(schema),
                    type_position.type_name().clone(),
                    key_directive_arguments.fields,
                )?);
            }
        }
        Ok(key_fields)
    }

    fn collect_fields_used_by_requires_directive(
        schema: &Valid<FederationSchema>,
        federation_spec_definition: &'static FederationSpecDefinition,
    ) -> Result<IndexSet<FieldDefinitionPosition>, FederationError> {
        let requires_directive_definition =
            federation_spec_definition.requires_directive_definition(schema)?;
        let requires_directive_referencers = schema
            .referencers
            .get_directive(&requires_directive_definition.name)?;

        let mut required_fields = IndexSet::default();
        for field_definition_position in &requires_directive_referencers.object_fields {
            let directives = &field_definition_position.get(schema.schema())?.directives;
            for requires_directive_application in
                directives.get_all(&requires_directive_definition.name)
            {
                let requires_directive_arguments = federation_spec_definition
                    .requires_directive_arguments(requires_directive_application)?;
                required_fields.extend(collect_target_fields_from_field_set(
                    unwrap_schema(schema),
                    field_definition_position.parent().type_name,
                    requires_directive_arguments.fields,
                )?);
            }
        }

        Ok(required_fields)
    }

    fn collect_fields_used_by_provides_directive(
        schema: &Valid<FederationSchema>,
        federation_spec_definition: &'static FederationSpecDefinition,
    ) -> Result<IndexSet<FieldDefinitionPosition>, FederationError> {
        let provides_directive_definition =
            federation_spec_definition.provides_directive_definition(schema)?;
        let provides_directive_referencers = schema
            .referencers
            .get_directive(&provides_directive_definition.name)?;

        let mut provided_fields = IndexSet::default();
        for field_definition_position in &provides_directive_referencers.object_fields {
            let field_definition = field_definition_position.get(schema.schema())?;
            let directives = &field_definition.directives;
            for provides_directive_application in
                directives.get_all(&provides_directive_definition.name)
            {
                let provides_directive_arguments = federation_spec_definition
                    .provides_directive_arguments(provides_directive_application)?;
                provided_fields.extend(collect_target_fields_from_field_set(
                    unwrap_schema(schema),
                    field_definition.ty.inner_named_type().clone(),
                    provides_directive_arguments.fields,
                )?);
            }
        }

        Ok(provided_fields)
    }

    fn collect_fields_used_by_context_directive(
        schema: &Valid<FederationSchema>,
        federation_spec_definition: &'static FederationSpecDefinition,
    ) -> Result<IndexSet<FieldDefinitionPosition>, FederationError> {
        let context_directive_definition =
            federation_spec_definition.context_directive_definition(schema)?;
        let from_context_directive_definition =
            federation_spec_definition.from_context_directive_definition(schema)?;

        // TODO: JS code short-circuits successfully if either of these doesn't exist
        let mut used_context_fields = IndexSet::default();
        let mut entry_points: HashMap<String, HashSet<CompositeTypeDefinitionPosition>> =
            HashMap::new();
        let context_directive_referencers = schema
            .referencers
            .get_directive(&context_directive_definition.name)?;
        // TODO: These loops are repetitive and brittle to adding new schema targets for a particular directive
        for interface_type_position in &context_directive_referencers.interface_types {
            let directives = &interface_type_position.get(schema.schema())?.directives;
            for context_directive_application in
                directives.get_all(&context_directive_definition.name)
            {
                let context = federation_spec_definition
                    .context_directive_arguments(context_directive_application)?
                    .name;
                if !entry_points.contains_key(context) {
                    entry_points.insert(context.to_string(), HashSet::new());
                }
                entry_points
                    .get_mut(context)
                    .expect("was just inserted")
                    .insert(interface_type_position.clone().into());
            }
        }
        for object_type_position in &context_directive_referencers.object_types {
            let directives = &object_type_position.get(schema.schema())?.directives;
            for context_directive_application in
                directives.get_all(&context_directive_definition.name)
            {
                let context = federation_spec_definition
                    .context_directive_arguments(context_directive_application)?
                    .name;
                if !entry_points.contains_key(context) {
                    entry_points.insert(context.to_string(), HashSet::new());
                }
                entry_points
                    .get_mut(context)
                    .expect("was just inserted")
                    .insert(object_type_position.clone().into());
            }
        }
        for union_type_position in &context_directive_referencers.union_types {
            let directives = &union_type_position.get(schema.schema())?.directives;
            for context_directive_application in
                directives.get_all(&context_directive_definition.name)
            {
                let context = federation_spec_definition
                    .context_directive_arguments(context_directive_application)?
                    .name;
                if !entry_points.contains_key(context) {
                    entry_points.insert(context.to_string(), HashSet::new());
                }
                entry_points
                    .get_mut(context)
                    .expect("was just inserted")
                    .insert(union_type_position.clone().into());
            }
        }

        let from_context_directive_referencers = schema
            .referencers
            .get_directive(&from_context_directive_definition.name)?;
        for argument_definition_position in &from_context_directive_referencers.directive_arguments
        {
            let directives = &argument_definition_position
                .get(schema.schema())?
                .directives;
            // TODO: JS used this below; are we using a wrong type name somewhere?
            // let type_ = argument_definition_position.parent();
            for from_context_directive_application in
                directives.get_all(&from_context_directive_definition.name)
            {
                let field_value = federation_spec_definition
                    .from_context_directive_arguments(from_context_directive_application)?
                    .field;
                if let Some((context, selection)) = parse_context(field_value) {
                    if let Some(entry_point) = entry_points.get(context) {
                        for context_type in entry_point {
                            used_context_fields.extend(collect_target_fields_from_field_set(
                                unwrap_schema(schema),
                                context_type.type_name().clone(),
                                selection,
                            )?);
                        }
                    }
                }
            }
        }

        Ok(used_context_fields)
    }

    fn collect_fields_used_to_satisfy_interface_constraints(
        schema: &Valid<FederationSchema>,
        used_fields: &mut IndexSet<FieldDefinitionPosition>,
    ) -> Result<(), FederationError> {
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
                        used_fields.insert(FieldDefinitionPosition::Object(
                            ObjectFieldDefinitionPosition {
                                type_name: object_type.name.clone(),
                                field_name: field_name.clone(),
                            },
                        ));
                    }
                }
            }
        }

        Ok(())
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
}

impl ExternalMetadata {
    fn new(
        schema: &Valid<FederationSchema>,
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

        Ok(Self {
            external_fields,
            fake_external_fields,
            fields_on_external_types,
        })
    }

    fn collect_external_fields(
        federation_spec_definition: &'static FederationSpecDefinition,
        schema: &Valid<FederationSchema>,
    ) -> Result<IndexSet<FieldDefinitionPosition>, FederationError> {
        let external_directive_definition = federation_spec_definition
            .external_directive_definition(schema)?
            .clone();

        let external_directive_referencers = schema
            .referencers
            .get_directive(&external_directive_definition.name)?;

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
        schema: &Valid<FederationSchema>,
    ) -> Result<IndexSet<FieldDefinitionPosition>, FederationError> {
        let mut fake_external_fields = IndexSet::default();
        let extends_directive_definition =
            federation_spec_definition.extends_directive_definition(schema)?;
        let key_directive_definition =
            federation_spec_definition.key_directive_definition(schema)?;
        let key_directive_referencers = schema
            .referencers
            .get_directive(&key_directive_definition.name)?;
        let mut key_type_positions: Vec<ObjectOrInterfaceTypeDefinitionPosition> = vec![];
        for object_type_position in &key_directive_referencers.object_types {
            key_type_positions.push(object_type_position.clone().into());
        }
        for interface_type_position in &key_directive_referencers.interface_types {
            key_type_positions.push(interface_type_position.clone().into());
        }
        for type_position in key_type_positions {
            let directives = match &type_position {
                ObjectOrInterfaceTypeDefinitionPosition::Object(pos) => {
                    &pos.get(schema.schema())?.directives
                }
                ObjectOrInterfaceTypeDefinitionPosition::Interface(pos) => {
                    &pos.get(schema.schema())?.directives
                }
            };
            let has_extends_directive = directives.has(&extends_directive_definition.name);
            for key_directive_application in directives.get_all(&key_directive_definition.name) {
                // PORT_NOTE: The JS codebase treats the "extend" GraphQL keyword as applying to
                // only the extension it's on, while it treats the "@extends" directive as applying
                // to all definitions/extensions in the subgraph. We accordingly do the same.
                if has_extends_directive
                    || key_directive_application.origin.extension_id().is_some()
                {
                    let key_directive_arguments = federation_spec_definition
                        .key_directive_arguments(key_directive_application)?;
                    fake_external_fields.extend(collect_target_fields_from_field_set(
                        unwrap_schema(schema),
                        type_position.type_name().clone(),
                        key_directive_arguments.fields,
                    )?);
                }
            }
        }
        Ok(fake_external_fields)
    }

    fn collect_fields_on_external_types(
        federation_spec_definition: &'static FederationSpecDefinition,
        schema: &Valid<FederationSchema>,
    ) -> Result<IndexSet<FieldDefinitionPosition>, FederationError> {
        let external_directive_definition = federation_spec_definition
            .external_directive_definition(schema)?
            .clone();

        let external_directive_referencers = schema
            .referencers
            .get_directive(&external_directive_definition.name)?;

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

    pub(crate) fn is_fake_external(
        &self,
        field_definition_position: &FieldDefinitionPosition,
    ) -> bool {
        self.fake_external_fields
            .contains(field_definition_position)
    }

    pub(crate) fn is_fully_external(&self, field: &FieldDefinitionPosition) -> bool {
        todo!()
    }

    pub(crate) fn is_partially_external(&self, field: &FieldDefinitionPosition) -> bool {
        todo!()
    }

    pub(crate) fn selects_any_external_field(&self, selection_set: &SelectionSet) -> bool {
        for selection in selection_set.selections.values() {
            if let Selection::Field(field_selection) = selection {
                if self.is_external(&field_selection.field.field_position) {
                    return true;
                }
            }
            if let Some(selection_set) = selection.selection_set() {
                if self.selects_any_external_field(selection_set) {
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use crate::schema::position::FieldDefinitionPosition;
    use crate::schema::position::ObjectFieldDefinitionPosition;
    use crate::schema::FederationSchema;
    use apollo_compiler::Name;

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

        assert_eq!(
            meta.shareable_fields.len(),
            4,
            "ensure no extra fields in {:?}",
            meta.shareable_fields
        );
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

        assert_eq!(
            meta.used_fields.len(),
            7,
            "ensure no extra fields in {:?}",
            meta.used_fields
        );
    }

    fn field(type_name: &str, field_name: &str) -> FieldDefinitionPosition {
        FieldDefinitionPosition::Object(ObjectFieldDefinitionPosition {
            type_name: Name::new_unchecked(type_name),
            field_name: Name::new_unchecked(field_name),
        })
    }
}
