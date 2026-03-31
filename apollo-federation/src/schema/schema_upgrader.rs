use std::collections::HashSet;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Value;
use apollo_compiler::collections::HashMap;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use either::Either;
use indexmap::IndexMap;
use indexmap::IndexSet;
use itertools::Itertools;
use tracing::instrument;
use tracing::trace;

use super::FederationSchema;
use super::TypeDefinitionPosition;
use super::field_set::collect_target_fields_from_field_set;
use super::position::FieldDefinitionPosition;
use super::position::HasAppliedDirectives;
use super::position::InterfaceFieldDefinitionPosition;
use super::position::InterfaceTypeDefinitionPosition;
use super::position::ObjectFieldDefinitionPosition;
use super::position::ObjectTypeDefinitionPosition;
use crate::error::CompositionError;
use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::schema::SchemaElement;
use crate::schema::SubgraphMetadata;
use crate::subgraph::SubgraphError;
use crate::subgraph::typestate::Expanded;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Upgraded;
use crate::subgraph::typestate::Validated;
use crate::supergraph::GRAPHQL_SUBSCRIPTION_TYPE_NAME;
use crate::supergraph::remove_inactive_requires_and_provides_from_subgraph;
use crate::utils::FallibleIterator;
use crate::utils::human_readable::human_readable_subgraph_names;

// TODO should this module be under subgraph mod?
#[derive(Debug)]
pub(crate) struct SchemaUpgrader {
    subgraphs: HashMap<String, Subgraph<Expanded>>,
    object_type_map: HashMap<Name, HashMap<String, TypeInfo>>,
}

#[derive(Clone, Debug)]
struct TypeInfo {
    pos: TypeDefinitionPosition,
    metadata: SubgraphMetadata,
}

#[derive(Debug)]
struct UpgradeMetadata {
    subgraph_name: String,
    key_directive_name: Option<Name>,
    requires_directive_name: Option<Name>,
    provides_directive_name: Option<Name>,
    extends_directive_name: Option<Name>,
    metadata: SubgraphMetadata,
    orphan_extension_types: HashSet<Name>,
}

impl UpgradeMetadata {
    /// Returns true if the given type name is an orphan type extension in this subgraph.
    /// - Orphan type implies that there is one or more extensions for the type, but no base
    ///   definition.
    fn is_orphan_extension_type(&self, type_name: &Name) -> bool {
        self.orphan_extension_types.contains(type_name)
    }
}

pub(crate) struct UpgradeResult {
    pub(crate) subgraph: Subgraph<Upgraded>,
    pub(crate) interfaces_with_key_removed: IndexSet<Name>,
}

impl SchemaUpgrader {
    pub(crate) fn new(subgraphs: &[Subgraph<Expanded>]) -> Self {
        let mut object_type_map: HashMap<Name, HashMap<String, TypeInfo>> = Default::default();
        for subgraph in subgraphs.iter() {
            for pos in subgraph.schema().get_types() {
                if matches!(
                    pos,
                    TypeDefinitionPosition::Object(_) | TypeDefinitionPosition::Interface(_)
                ) {
                    object_type_map
                        .entry(pos.type_name().clone())
                        .or_default()
                        .insert(
                            subgraph.name.clone(),
                            TypeInfo {
                                pos: pos.clone(),
                                metadata: subgraph.metadata().clone(), // TODO: Prefer not to clone
                            },
                        );
                }
            }
        }

        let subgraphs: HashMap<String, Subgraph<Expanded>> = subgraphs
            .iter()
            .map(|subgraph| (subgraph.name.clone(), subgraph.clone()))
            .collect();
        SchemaUpgrader {
            subgraphs,
            object_type_map,
        }
    }

    fn get_subgraph_by_name(&self, name: &String) -> Option<&Subgraph<Expanded>> {
        self.subgraphs.get(name)
    }

    pub(crate) fn upgrade(
        &self,
        subgraph: Subgraph<Expanded>,
    ) -> Result<UpgradeResult, SubgraphError> {
        let subgraph_name = subgraph.name.clone();
        self.upgrade_inner(subgraph)
            .map_err(|e| SubgraphError::new_without_locations(subgraph_name, e))
    }

    pub(crate) fn upgrade_inner(
        &self,
        subgraph: Subgraph<Expanded>,
    ) -> Result<UpgradeResult, FederationError> {
        let orphan_extensions = subgraph.state.orphan_extension_types().clone();
        // PORT NOTE: this logic was done in JS SchemaUpgrader constructor
        trace!("upgrade_inner: into fed 2 subgraph");
        let mut subgraph = subgraph.into_fed_2_subgraph()?;

        // Run pre-upgrade validations to check for issues that would prevent upgrade
        let mut upgrade_metadata = UpgradeMetadata {
            subgraph_name: subgraph.name.clone(),
            key_directive_name: subgraph.key_directive_name()?.clone(),
            requires_directive_name: subgraph.requires_directive_name()?.clone(),
            provides_directive_name: subgraph.provides_directive_name()?.clone(),
            extends_directive_name: subgraph.extends_directive_name()?.clone(),
            metadata: subgraph.metadata().clone(),
            orphan_extension_types: orphan_extensions,
        };

        self.pre_upgrade_validations(&upgrade_metadata, &subgraph)?;
        let schema = subgraph.state.federation_schema_mut();

        // Fix federation directive arguments (fields) to ensure they're proper strings
        // Note: Implementation simplified for compilation purposes
        self.fix_federation_directives_arguments(schema)?;

        trace!("upgrade_inner: removing unnecessary @external");
        self.remove_external_on_interface(&mut upgrade_metadata, schema);

        self.remove_external_on_object_types(&mut upgrade_metadata, schema);

        // Note that we remove all external on type extensions first, so we don't have to care about it later in @key, @provides and @requires.
        self.remove_external_on_type_extensions(&mut upgrade_metadata, schema)?;

        trace!("upgrade_inner: fixing provides and requires");
        self.fix_inactive_provides_and_requires(schema)?;

        self.remove_type_extensions(&upgrade_metadata, schema)?;

        let interface_key_types =
            self.remove_directives_on_interface(&upgrade_metadata, schema)?;

        // Note that this rule rely on being after `remove_directives_on_interface` in practice (in that it doesn't check interfaces).
        self.remove_provides_on_non_composite(&upgrade_metadata, schema)?;

        // Note that this should come _after_ all the other changes that may remove/update federation directives, since those may create unused
        // externals. Which is why this is toward  the end.
        self.remove_unused_externals(&upgrade_metadata, schema)?;

        trace!("upgrade_inner: add shareable");
        self.add_shareable(&upgrade_metadata, schema)?;

        self.remove_tag_on_external(&upgrade_metadata, schema)?;

        // Some type extensions are converted to definitions in the `remove_type_extensions`.
        // We need to update the orphan extension types accordingly.
        let filtered_orphan_extension_types =
            Self::filter_orphan_extension_types(upgrade_metadata.orphan_extension_types, schema);
        subgraph
            .state
            .update_orphan_extension_types(filtered_orphan_extension_types);

        trace!("upgrade_inner: complete");
        Ok(UpgradeResult {
            subgraph,
            interfaces_with_key_removed: interface_key_types,
        })
    }

    /// Compute a new `orphan_extension_types` with only types that still have an extension in the upgraded schema.
    fn filter_orphan_extension_types(
        orphan_extension_types: HashSet<Name>,
        schema: &FederationSchema,
    ) -> HashSet<Name> {
        orphan_extension_types
            .into_iter()
            .filter(|type_name| {
                schema.try_get_type((*type_name).clone()).is_some_and(|ty| {
                    let Ok(ty) = ty.get(schema.schema()) else {
                        return false;
                    };
                    ty.has_extension_elements()
                })
            })
            .collect()
    }

    // integrates checkForExtensionWithNoBase from the JS code
    fn pre_upgrade_validations(
        &self,
        upgrade_metadata: &UpgradeMetadata,
        subgraph: &Subgraph<Upgraded>,
    ) -> Result<(), FederationError> {
        let schema = subgraph.schema();

        // Iterate through all types and check if they're federation type extensions without a base
        for type_pos in schema.get_types() {
            if self.is_root_type_extension(&type_pos, upgrade_metadata, schema)
                || !self.is_federation_type_extension(&type_pos, upgrade_metadata, schema)?
            {
                continue;
            }

            // Get the type name based on the type position
            let type_name = type_pos.type_name();

            // Check if any other subgraph has a proper definition for this type
            let has_non_extension_definition = self
                .object_type_map
                .get(type_name)
                .map(|subgraph_types| {
                    subgraph_types
                        .iter()
                        .filter(|(subgraph_name, _)| {
                            // Fixed: dereference the string for comparison
                            subgraph_name.as_str() != subgraph.name.as_str()
                        })
                        .fallible_any(|(other_name, type_info)| {
                            let Some(other_subgraph) = self.get_subgraph_by_name(other_name) else {
                                return Ok(false);
                            };
                            let extended_type =
                                type_info.pos.get(other_subgraph.schema().schema())?;
                            Ok::<bool, FederationError>(
                                !other_subgraph.is_orphan_extension_type(extended_type.name()),
                            )
                        })
                })
                .unwrap_or(Ok(false))?;

            if !has_non_extension_definition {
                return Err(SingleFederationError::ExtensionWithNoBase {
                    message: format!("Type \"{type_name}\" is an extension type, but there is no type definition for \"{type_name}\" in any subgraph.")
                }.into());
            }
        }

        Ok(())
    }

    // Either we have a string, or we have a list of strings that we need to combine
    fn make_fields_string_if_not(arg: &Node<Value>) -> Result<Option<String>, FederationError> {
        if let Some(arg_list) = arg.as_list() {
            // Collect all strings from the list
            let combined = arg_list
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(" ");

            return Ok(Some(combined));
        }
        if let Some(enum_value) = arg.as_enum() {
            return Ok(Some(enum_value.as_str().to_string()));
        }
        Ok(None)
    }

    fn fix_federation_directives_arguments(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        // both @provides and @requires will only have an object_fields referencer
        for directive_name in ["requires", "provides"] {
            let referencers = schema.referencers().get_directive(directive_name);
            for field in &referencers.object_fields.clone() {
                let field_type = field.make_mut(&mut schema.schema)?.make_mut();

                for directive in field_type.directives.get_all_mut(directive_name) {
                    if let Some(arg) = directive
                        .make_mut()
                        .specified_argument_by_name_mut("fields")
                        && let Some(new_fields_string) = Self::make_fields_string_if_not(arg)?
                    {
                        *arg.make_mut() = Value::String(new_fields_string);
                    }
                }
            }
        }

        // now do the exact same thing for @key. The difference is that the directive location will be object_types
        // rather than object_fields
        let referencers = schema.referencers().get_directive("key");
        for field in &referencers.object_types.clone() {
            let field_type = field.make_mut(&mut schema.schema)?.make_mut();

            for directive in field_type.directives.iter_mut().filter(|d| d.name == "key") {
                if let Some(arg) = directive
                    .make_mut()
                    .specified_argument_by_name_mut("fields")
                    && let Some(new_fields_string) = Self::make_fields_string_if_not(arg)?
                {
                    *arg.make_mut() = Value::String(new_fields_string);
                }
            }
        }
        Ok(())
    }

    fn remove_external_on_interface(
        &self,
        upgrade_metadata: &mut UpgradeMetadata,
        schema: &mut FederationSchema,
    ) {
        let Ok(external_directive) = upgrade_metadata
            .metadata
            .federation_spec_definition()
            .external_directive_definition(schema)
        else {
            return;
        };
        let mut to_delete: Vec<(InterfaceFieldDefinitionPosition, Node<Directive>)> = vec![];
        for (itf_name, ty) in schema.schema().types.iter() {
            let ExtendedType::Interface(itf) = ty else {
                continue;
            };
            let interface_pos = InterfaceTypeDefinitionPosition::new(itf_name.clone());
            for (field_name, field) in &itf.fields {
                let pos = interface_pos.field(field_name.clone());
                let external_directive = field
                    .node
                    .directives
                    .iter()
                    .find(|d| d.name == external_directive.name);
                if let Some(external_directive) = external_directive {
                    to_delete.push((pos, external_directive.clone()));
                }
            }
        }
        for (pos, directive) in to_delete {
            pos.remove_directive(schema, &directive);
            upgrade_metadata
                .metadata
                .remove_external_field(&FieldDefinitionPosition::Interface(pos));
        }
    }

    fn remove_external_on_object_types(
        &self,
        upgrade_metadata: &mut UpgradeMetadata,
        schema: &mut FederationSchema,
    ) {
        let Ok(external_directive) = upgrade_metadata
            .metadata
            .federation_spec_definition()
            .external_directive_definition(schema)
        else {
            return;
        };
        let mut to_delete: Vec<(ObjectTypeDefinitionPosition, Component<Directive>)> = vec![];
        let mut fields_on_external_type: Vec<ObjectFieldDefinitionPosition> = vec![];
        for (obj_name, ty) in &schema.schema().types {
            let ExtendedType::Object(_) = ty else {
                continue;
            };

            let object_pos = ObjectTypeDefinitionPosition::new(obj_name.clone());
            let directives = object_pos.get_applied_directives(schema, &external_directive.name);
            if !directives.is_empty() {
                if let Ok(fields) = object_pos.fields(schema.schema()) {
                    fields_on_external_type.extend(fields);
                }
                to_delete.push((object_pos, directives[0].clone()));
            }
        }
        for (pos, directive) in to_delete {
            pos.remove_directive(schema, &directive);
        }
        upgrade_metadata
            .metadata
            .remove_external_type_fields(fields_on_external_type);
    }

    fn remove_external_on_type_extensions(
        &self,
        upgrade_metadata: &mut UpgradeMetadata,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        let types: Vec<_> = schema.get_types().collect();
        let key_directive = upgrade_metadata
            .metadata
            .federation_spec_definition()
            .key_directive_definition(schema)?;
        let external_directive = upgrade_metadata
            .metadata
            .federation_spec_definition()
            .external_directive_definition(schema)?;

        let mut to_remove = vec![];
        for ty in &types {
            if !ty.is_composite_type()
                || (!self.is_federation_type_extension(ty, upgrade_metadata, schema)?
                    && !self.is_root_type_extension(ty, upgrade_metadata, schema))
            {
                continue;
            }

            let key_applications = ty.get_applied_directives(schema, &key_directive.name);
            if !key_applications.is_empty() {
                for directive in key_applications {
                    let args = upgrade_metadata
                        .metadata
                        .federation_spec_definition()
                        .key_directive_arguments(directive)?;
                    for field in collect_target_fields_from_field_set(
                        Valid::assume_valid_ref(schema.schema()),
                        ty.type_name().clone(),
                        args.fields,
                        false,
                    )? {
                        let external =
                            field.get_applied_directives(schema, &external_directive.name);
                        if !external.is_empty() {
                            to_remove.push((field.clone(), external[0].clone()));
                        }
                    }
                }
            } else {
                // ... but if the extension does _not_ have a key, then if the extension has a field that is
                // part of the _1st_ key on the subgraph owning the type, then this field is not considered
                // external (yes, it's pretty damn random, and it's even worst in that even if the extension
                // does _not_ have the "field of the _1st_ key on the subgraph owning the type", then the
                // query planner will still request it to the subgraph, generating an invalid query; but
                // we ignore that here). Note however that because other subgraphs may have already been
                // upgraded, we don't know which is the "type owner", so instead we look up at the first
                // key of every other subgraph. It's not 100% what fed1 does, but we're in very-strange
                // case territory in the first place, so this is probably good enough (that is, there is
                // customer schema for which what we do here matter but not that I know of for which it's
                // not good enough).
                let Some(entries) = self.object_type_map.get(ty.type_name()) else {
                    continue;
                };
                for (subgraph_name, info) in entries.iter() {
                    if subgraph_name == upgrade_metadata.subgraph_name.as_str() {
                        continue;
                    }
                    let Some(other_schema) = self.get_subgraph_by_name(subgraph_name) else {
                        continue;
                    };
                    let keys_in_other = info.pos.get_applied_directives(
                        other_schema.schema(),
                        &info
                            .metadata
                            .federation_spec_definition()
                            .key_directive_definition(other_schema.schema())?
                            .name,
                    );
                    if keys_in_other.is_empty() {
                        continue;
                    }
                    let directive = keys_in_other[0];
                    let args = upgrade_metadata
                        .metadata
                        .federation_spec_definition()
                        .key_directive_arguments(directive)?;
                    for field in collect_target_fields_from_field_set(
                        Valid::assume_valid_ref(schema.schema()),
                        ty.type_name().clone(),
                        args.fields,
                        false,
                    )? {
                        if TypeDefinitionPosition::from(field.parent()) != info.pos {
                            continue;
                        }
                        let external =
                            field.get_applied_directives(schema, &external_directive.name);
                        if !external.is_empty() {
                            to_remove.push((field.clone(), external[0].clone()));
                        }
                    }
                }
            }
        }

        for (pos, directive) in &to_remove {
            pos.remove_directive(schema, directive);
            upgrade_metadata.metadata.remove_external_field(pos);
        }

        Ok(())
    }

    fn fix_inactive_provides_and_requires(
        &self,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        let cloned_schema = schema.clone();
        remove_inactive_requires_and_provides_from_subgraph(
            &cloned_schema, // TODO: I don't know what this value should be
            schema,
        )
    }

    fn remove_type_extensions(
        &self,
        upgrade_metadata: &UpgradeMetadata,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        let types: Vec<_> = schema.get_types().collect();
        for ty in types {
            if !self.is_federation_type_extension(&ty, upgrade_metadata, schema)?
                && !self.is_root_type_extension(&ty, upgrade_metadata, schema)
            {
                continue;
            }
            ty.remove_extensions(schema)?;
        }
        Ok(())
    }

    /// Whether the type represents a type extension in the sense of federation 1.
    /// That is, type extensions are a thing in GraphQL, but federation 1 overloads the notion for entities. This method
    /// returns true if the type is used in the federation 1 sense of an extension.
    /// We recognize federation 1 type extensions as type extensions that:
    ///  1. are on object types or interface types (note that federation 1 doesn't really handle interface type extensions properly but it "accepts" them
    ///     so we do it here too).
    ///  2. do not have a definition for the same type in the same subgraph (this is a GraphQL extension otherwise).
    ///
    /// Not that type extensions in federation 1 generally have a @key, but in reality the code considers something a type extension even without
    /// it (which one could argue is an unintended bug of fed1 since this leads to various problems). So, we don't check for the presence of @key here.
    fn is_federation_type_extension(
        &self,
        ty: &TypeDefinitionPosition,
        upgrade_metadata: &UpgradeMetadata,
        schema: &FederationSchema,
    ) -> Result<bool, FederationError> {
        let type_ = ty.get(schema.schema())?;
        let has_extend = upgrade_metadata
            .extends_directive_name
            .as_ref()
            .is_some_and(|extends| type_.directives().has(extends.as_str()));
        let is_orphan_extension = upgrade_metadata.is_orphan_extension_type(type_.name());

        // `is_orphan_extension` implies that there is at least one extension for the type, so we
        // don't need to check for `type_.has_extension_elements()`.
        Ok((type_.is_object() || type_.is_interface()) && (has_extend || is_orphan_extension))
    }

    /// Whether the type is a root type but is declared only as an extension, which federation 1 actually accepts.
    fn is_root_type_extension(
        &self,
        pos: &TypeDefinitionPosition,
        upgrade_metadata: &UpgradeMetadata,
        schema: &FederationSchema,
    ) -> bool {
        if !matches!(pos, TypeDefinitionPosition::Object(_)) || !Self::is_root_type(schema, pos) {
            return false;
        }
        let Ok(ty) = pos.get(schema.schema()) else {
            return false;
        };
        let has_extends_directive = upgrade_metadata
            .extends_directive_name
            .as_ref()
            .is_some_and(|extends| ty.directives().has(extends.as_str()));
        let is_orphan_extension = upgrade_metadata.is_orphan_extension_type(ty.name());

        has_extends_directive || is_orphan_extension
    }

    fn is_root_type(schema: &FederationSchema, ty: &TypeDefinitionPosition) -> bool {
        schema.is_root_type(ty.type_name())
    }

    fn remove_directives_on_interface(
        &self,
        upgrade_metadata: &UpgradeMetadata,
        schema: &mut FederationSchema,
    ) -> Result<IndexSet<Name>, FederationError> {
        let mut interface_key_types = IndexSet::new();
        if let Some(key) = &upgrade_metadata.key_directive_name {
            for pos in schema
                .referencers()
                .get_directive(key)
                .interface_types
                .clone()
            {
                interface_key_types.insert(pos.type_name.clone());
                pos.remove_directive_name(schema, key);

                let fields: Vec<_> = pos.fields(schema.schema())?.collect();
                for field in fields {
                    if let Some(provides) = &upgrade_metadata.provides_directive_name {
                        field.remove_directive_name(schema, provides);
                    }
                    if let Some(requires) = &upgrade_metadata.requires_directive_name {
                        field.remove_directive_name(schema, requires);
                    }
                }
            }
        }

        Ok(interface_key_types)
    }

    fn remove_provides_on_non_composite(
        &self,
        upgrade_metadata: &UpgradeMetadata,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        let Some(provides_directive) = &upgrade_metadata.provides_directive_name else {
            return Ok(());
        };
        let Some(targets) = schema.referencers().directives.get(provides_directive) else {
            return Ok(());
        };
        let mut candidates: IndexSet<ObjectFieldDefinitionPosition> = IndexSet::default();
        for obj_field_pos in targets.object_fields.iter() {
            let field = obj_field_pos.get(schema.schema())?;
            let return_type = field.ty.inner_named_type();
            if schema
                .try_get_type(return_type.clone())
                .is_some_and(|t| !t.is_composite_type())
            {
                candidates.insert(obj_field_pos.clone());
            }
        }

        for candidate in candidates {
            candidate
                .make_mut(schema.schema_mut())?
                .make_mut()
                .directives
                .retain(|d| d.name != provides_directive.as_str());
        }
        Ok(())
    }

    fn remove_unused_externals(
        &self,
        upgrade_metadata: &UpgradeMetadata,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        let mut error = MultipleFederationErrors::new();
        let mut fields_to_remove: HashSet<ObjectFieldDefinitionPosition> = HashSet::new();
        let mut types_to_remove: HashSet<ObjectTypeDefinitionPosition> = HashSet::new();
        for type_ in schema.get_types() {
            // @external is already removed from interfaces so we only need to process objects
            if let Ok(pos) = ObjectTypeDefinitionPosition::try_from(type_) {
                let mut has_fields = false;
                for field in pos.fields(schema.schema())? {
                    let field_def = FieldDefinitionPosition::from(field.clone());
                    let metadata = &upgrade_metadata.metadata;
                    if metadata.is_field_external(&field_def) && !metadata.is_field_used(&field_def)
                    {
                        fields_to_remove.insert(field);
                    } else {
                        // Any non-removed field will set this boolean
                        has_fields = true;
                    }
                }
                if !has_fields {
                    let is_referenced = schema
                        .referencers
                        .object_types
                        .get(&pos.type_name)
                        .is_some_and(|r| r.len() > 0);
                    if is_referenced {
                        error
                            .errors
                            .push(SingleFederationError::TypeWithOnlyUnusedExternal {
                                type_name: pos.type_name.clone(),
                            });
                    } else {
                        types_to_remove.insert(pos);
                    }
                }
            }
        }

        for field in fields_to_remove {
            field.remove(schema)?;
        }
        for type_ in types_to_remove {
            type_.remove(schema)?;
        }
        error.into_result()
    }

    fn add_shareable(
        &self,
        upgrade_metadata: &UpgradeMetadata,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        let Some(key_directive_name) = &upgrade_metadata.key_directive_name else {
            return Ok(());
        };

        let shareable_directive_name = upgrade_metadata
            .metadata
            .federation_spec_definition()
            .shareable_directive_definition(schema)?
            .name
            .clone();

        let mut fields_to_add_shareable = vec![];
        let mut types_to_add_shareable = vec![];
        for type_pos in schema.get_types() {
            let has_key_directive = type_pos.has_applied_directive(schema, key_directive_name);
            let is_root_type = Self::is_root_type(schema, &type_pos);
            let TypeDefinitionPosition::Object(obj_pos) = type_pos else {
                continue;
            };
            let obj_name = &obj_pos.type_name;
            // Skip Subscription root type - no shareable needed
            if obj_pos.type_name == GRAPHQL_SUBSCRIPTION_TYPE_NAME {
                continue;
            }
            if has_key_directive || is_root_type {
                for field in obj_pos.fields(schema.schema())? {
                    let obj_field = FieldDefinitionPosition::Object(field.clone());
                    if upgrade_metadata.metadata.is_field_shareable(&obj_field) {
                        continue;
                    }
                    let Some(entries) = self.object_type_map.get(obj_name) else {
                        continue;
                    };
                    let type_in_other_subgraphs = entries.iter().any(|(subgraph_name, info)| {
                        let field_exists = self
                            .get_subgraph_by_name(subgraph_name)
                            .unwrap()
                            .schema()
                            .schema()
                            .type_field(&field.type_name, &field.field_name)
                            .is_ok();

                        if (subgraph_name != upgrade_metadata.subgraph_name.as_str())
                            && field_exists
                            && (!info.metadata.is_field_external(&obj_field)
                                || info.metadata.is_field_partially_external(&obj_field))
                        {
                            return true;
                        }
                        false
                    });
                    if type_in_other_subgraphs
                        && !obj_field.has_applied_directive(schema, &shareable_directive_name)
                    {
                        fields_to_add_shareable.push(field.clone());
                    }
                }
            } else {
                let Some(entries) = self.object_type_map.get(obj_name) else {
                    continue;
                };
                let type_in_other_subgraphs = entries.iter().any(|(subgraph_name, _info)| {
                    if subgraph_name != upgrade_metadata.subgraph_name.as_str() {
                        return true;
                    }
                    false
                });
                if type_in_other_subgraphs
                    && !obj_pos.has_applied_directive(schema, &shareable_directive_name)
                {
                    types_to_add_shareable.push(obj_pos.clone());
                }
            }
        }
        for pos in &fields_to_add_shareable {
            pos.insert_directive(
                schema,
                Node::new(Directive {
                    name: shareable_directive_name.clone(),
                    arguments: vec![],
                }),
            )?;
        }
        for pos in &types_to_add_shareable {
            pos.insert_directive(
                schema,
                Component::new(Directive {
                    name: shareable_directive_name.clone(),
                    arguments: vec![],
                }),
            )?;
        }
        Ok(())
    }

    fn remove_tag_on_external(
        &self,
        upgrade_metadata: &UpgradeMetadata,
        schema: &mut FederationSchema,
    ) -> Result<(), FederationError> {
        let applications = schema.tag_directive_applications()?;
        let mut to_delete: Vec<(FieldDefinitionPosition, Node<Directive>)> = vec![];

        applications
            .into_iter()
            .filter_map(Result::ok)
            .filter_map(|application| {
                FieldDefinitionPosition::try_from(application.target.clone())
                    .ok()
                    .map(|target| (target, application))
            })
            .filter(|(target, _)| upgrade_metadata.metadata.is_field_external(target))
            .for_each(|(target, application)| {
                let used_in_other_definitions = self
                    .subgraphs
                    .iter()
                    // skip self
                    .filter(|(name, _)| &upgrade_metadata.subgraph_name != *name)
                    // check to see if the field is external in the other subgraphs
                    .filter(|(_, other_subgraph)| {
                        other_subgraph
                            .schema()
                            .subgraph_metadata
                            .as_ref()
                            .is_some_and(|other_metadata| {
                                !other_metadata.is_field_external(&target)
                            })
                    })
                    // at this point, we need to check to see if there is a @tag directive on the other subgraph that matches the current application
                    .fallible_any::<FederationError, _>(|(_, other_subgraph)| {
                        Ok(other_subgraph
                            .schema()
                            .tag_directive_applications()?
                            .iter()
                            .any(|other_application_result| {
                                (*other_application_result).as_ref().is_ok_and(
                                    |other_tag_directive| {
                                        application.target == other_tag_directive.target
                                            && application.arguments.name
                                                == other_tag_directive.arguments.name
                                    },
                                )
                            }))
                    })
                    .is_ok_and(|found| found);
                if used_in_other_definitions {
                    // this @tag should be removed
                    to_delete.push((target, application.directive.clone()));
                }
            });

        for (pos, directive) in to_delete {
            match pos {
                FieldDefinitionPosition::Object(target) => {
                    target.remove_directive(schema, &directive);
                }
                FieldDefinitionPosition::Interface(target) => {
                    target.remove_directive(schema, &directive);
                }
                FieldDefinitionPosition::Union(_target) => {
                    todo!();
                }
            }
        }
        Ok(())
    }
}

/// Upgrade subgraphs if necessary without validation.
// PORT_NOTE: This corresponds to `upgradeSubgraphsIfNecessary` function in JS.
#[instrument(skip(subgraphs))]
#[allow(clippy::type_complexity)]
fn inner_upgrade_subgraphs_if_necessary(
    subgraphs: Vec<Subgraph<Expanded>>,
) -> Result<Vec<Either<Subgraph<Expanded>, Subgraph<Upgraded>>>, Vec<CompositionError>> {
    let all_fed_2 = subgraphs
        .iter()
        .all(|subgraph| subgraph.metadata().is_fed_2_schema());
    // Maps type name → set of subgraph names with @interfaceObject on that type (fed2 subgraphs).
    let mut fed2_interface_object_types_to_subgraphs: IndexMap<Name, IndexSet<String>> =
        IndexMap::new();
    // Maps type name → set of subgraph names with interface @key on that type (fed1 subgraphs).
    let mut fed1_interface_key_types_to_subgraphs: IndexMap<Name, IndexSet<String>> =
        IndexMap::new();
    let mut errors: Vec<CompositionError> = vec![];
    let schema_upgrader: SchemaUpgrader = SchemaUpgrader::new(&subgraphs);
    let upgraded_subgraphs = subgraphs
        .into_iter()
        .map(|subgraph| {
            if !subgraph.metadata().is_fed_2_schema() {
                let result = schema_upgrader.upgrade(subgraph)?;
                for type_name in result.interfaces_with_key_removed {
                    fed1_interface_key_types_to_subgraphs
                        .entry(type_name)
                        .or_default()
                        .insert(result.subgraph.name.clone());
                }
                Ok(Either::Right(result.subgraph))
            } else {
                if !all_fed_2 {
                    subgraph
                        .metadata()
                        .interface_object_types()
                        .iter()
                        .for_each(|intf_object| {
                            fed2_interface_object_types_to_subgraphs
                                .entry(intf_object.clone())
                                .or_default()
                                .insert(subgraph.name.clone());
                        });
                }
                Ok(Either::Left(subgraph))
            }
        })
        .filter_map(|r| {
            r.map_err(|e: SubgraphError| errors.extend(e.to_composition_errors()))
                .ok()
        })
        .collect();

    for (intf_object_name, subgraphs_using_interface_object) in &fed2_interface_object_types_to_subgraphs {
        if let Some(subgraphs_with_interface_keys_removed) =
            fed1_interface_key_types_to_subgraphs.get(intf_object_name)
        {
            errors.push(CompositionError::InterfaceObjectUsageError {
                message: format!(
                    "The @interfaceObject directive is used on type \"{intf_object_name}\" in {}, \
                    which requires other subgraphs to resolve its type name via an interface @key. \
                    However, @key on an interface in a federation 1 subgraph does not mean it can \
                    fulfill the __typename-resolution requirement that @interfaceObject depends on. \
                    For {}, either upgrade them to federation 2 subgraphs or remove @key from the type.",
                    human_readable_subgraph_names(subgraphs_using_interface_object.iter()),
                    human_readable_subgraph_names(subgraphs_with_interface_keys_removed.iter()),
                ),
            });
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(upgraded_subgraphs)
}

/// Upgrade subgraphs if necessary and validate the result.
/// - Fed 2 input subgraphs are not upgraded.
/// - Also, normalizes root types if necessary.
/// - Unchanged subgraphs are returned as-is.
// PORT_NOTE: In JS, this returns upgraded subgraphs along with a set of messages about what changed.
// However, those messages were never used, so we have omitted them here.
#[instrument(skip(subgraphs))]
pub fn upgrade_subgraphs_if_necessary(
    subgraphs: Vec<Subgraph<Expanded>>,
) -> Result<Vec<Subgraph<Validated>>, Vec<CompositionError>> {
    let mut errors: Vec<CompositionError> = vec![];
    let subgraphs = subgraphs
        .into_iter()
        .filter_map(|sg| match sg.normalize_root_types() {
            Ok(s) => Some(s),
            Err(e) => {
                errors.extend(e.to_composition_errors());
                None
            }
        })
        .collect_vec();

    // Upgrade subgraphs (if necessary)
    let upgraded = inner_upgrade_subgraphs_if_necessary(subgraphs)?;

    // Validate subgraphs (if either upgraded or normalized)
    let validated: Vec<Subgraph<Validated>> = upgraded
        .into_iter()
        .filter_map(|subgraph| match subgraph {
            Either::Left(s) => {
                // This subgraph was not upgraded nor normalized in this function, which implies
                // this subgraph is originally Fed v2. Since Fed v2 schemas are already fully
                // validated in the `expand_links` method, it's safe to transition to the
                // `Validated` state.
                Some(s.assume_validated())
            }
            Either::Right(s) => match s.validate() {
                Ok(s) => Some(s),
                Err(e) => {
                    errors.extend(e.to_composition_errors());
                    None
                }
            },
        })
        .collect();

    if errors.is_empty() {
        Ok(validated)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::coord;
    use test_log::test;

    use super::*;

    const FEDERATION2_LINK_WITH_AUTO_EXPANDED_IMPORTS_UPGRADED: &str = r#"@link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.4", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject"])"#;

    #[test]
    fn upgrades_complex_schema() {
        let s1 = Subgraph::parse(
            "s1",
            "",
            r#"
            type Query {
                products: [Product!]! @provides(fields: "upc description")
            }

            interface I @key(fields: "upc") {
                upc: ID!
                description: String @external
            }

            extend type Product implements I @key(fields: "upc") {
                upc: ID! @external
                name: String @external
                inventory: Int @requires(fields: "upc")
                description: String @external
            }

            # A type with a genuine 'graphqQL' extension, to ensure the extend don't get removed.
            type Random {
                x: Int
            }

            extend type Random {
                y: Int
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        // Note that no changes are really expected on that 2nd schema: it is just there to make the example not throw due to
        // then Product type extension having no "base".
        let s2 = Subgraph::parse(
            "s2",
            "",
            r#"
            type Product @key(fields: "upc") {
              upc: ID!
              name: String
              description: String
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let [s1, _s2]: [Subgraph<_>; 2] = upgrade_subgraphs_if_necessary(vec![s1, s2])
            .expect("upgrades schema")
            .try_into()
            .expect("Expected 2 elements");

        insta::assert_snapshot!(
            s1.schema_string(), @r###"
        schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.4", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject"]) {
          query: Query
        }

        directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

        directive @requires(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @provides(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @external(reason: String) on OBJECT | FIELD_DEFINITION

        directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

        directive @extends on OBJECT | INTERFACE

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @shareable repeatable on OBJECT | FIELD_DEFINITION

        directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

        directive @override(from: String!) on FIELD_DEFINITION

        directive @composeDirective(name: String) repeatable on SCHEMA

        directive @interfaceObject on OBJECT

        type Query {
          products: [Product!]! @provides(fields: "description")
          _entities(representations: [_Any!]!): [_Entity]! @shareable
          _service: _Service! @shareable
        }

        interface I {
          upc: ID!
          description: String
        }

        type Random {
          x: Int
        }

        extend type Random {
          y: Int
        }

        type Product implements I @key(fields: "upc") {
          upc: ID!
          inventory: Int
          description: String @external
        }

        union _Entity = Product

        scalar _Any

        type _Service @shareable {
          sdl: String
        }

        scalar federation__FieldSet

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

        scalar link__Import
        "###
        );
    }

    #[test]
    #[ignore]
    fn update_federation_directive_non_string_arguments() {
        let s = Subgraph::parse(
            "s",
            "",
            r#"
            type Query {
                a: A
            }

            type A @key(fields: id) @key(fields: ["id", "x"]) {
                id: String
                x: Int
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let [s]: [Subgraph<_>; 1] = upgrade_subgraphs_if_necessary(vec![s])
            .expect("upgrades schema")
            .try_into()
            .expect("Expected 1 element");

        insta::assert_snapshot!(
            s.schema_string(),
            r#"
            schema
                FEDERATION2_LINK_WITH_AUTO_EXPANDED_IMPORTS_UPGRADED
            {
                query: Query
            }

            type Query {
                a: A
            }

            type A @key(fields: "id") @key(fields: "id x") {
                id: String
                x: Int
            }
        "#
            .replace(
                "FEDERATION2_LINK_WITH_AUTO_EXPANDED_IMPORTS_UPGRADED",
                FEDERATION2_LINK_WITH_AUTO_EXPANDED_IMPORTS_UPGRADED
            )
        );
    }

    #[test]
    fn remove_tag_on_external_field_if_found_on_definition() {
        let s1 = Subgraph::parse(
            "s1",
            "",
            r#"
            type Query {
                a: A @provides(fields: "y")
            }

            type A @key(fields: "id") {
                id: String
                x: Int
                y: Int @external @tag(name: "a tag")
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let s2 = Subgraph::parse(
            "s2",
            "",
            r#"
            type A @key(fields: "id") {
                id: String
                y: Int @tag(name: "a tag")
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let [s1, s2]: [Either<_, _>; 2] = inner_upgrade_subgraphs_if_necessary(vec![s1, s2])
            .expect("upgrades schema")
            .try_into()
            .expect("Expected 2 elements");

        let s1 = s1.expect_right("to be upgraded");
        let s2 = s2.expect_right("to be upgraded");
        let type_a_in_s1 = s1
            .schema()
            .schema()
            .get_object("A")
            .unwrap()
            .fields
            .get("y")
            .unwrap();
        let type_a_in_s2 = s2
            .schema()
            .schema()
            .get_object("A")
            .unwrap()
            .fields
            .get("y")
            .unwrap();

        assert_eq!(type_a_in_s1.directives.get_all("tag").count(), 0);
        assert_eq!(
            type_a_in_s2
                .directives
                .get_all("tag")
                .map(|d| d.to_string())
                .collect::<Vec<_>>(),
            vec![r#"@tag(name: "a tag")"#]
        );
    }

    #[test]
    fn reject_interface_object_usage_if_not_all_subgraphs_are_fed2() {
        // This test validates that when a fed2 subgraph uses @interfaceObject on a type that also
        // has an interface @key in a fed1 subgraph, we produce a targeted error. @key on an
        // interface in a Fed 1 subgraph does not indicate it can resolve type names for that
        // interface via _entities, which is required for @interfaceObject to work correctly,
        // so we reject this combination explicitly.

        let s1 = Subgraph::parse("s1", "", r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.3", import: [ "@key", "@interfaceObject"])

            type Query {
                a: A
            }

            type A @key(fields: "id") @interfaceObject {
                id: String
                x: Int
            }
        "#)
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let s2 = Subgraph::parse(
            "s2",
            "",
            r#"
            interface A @key(fields: "id") {
                id: String
                y: Int
            }

            type X implements A @key(fields: "id") {
                id: String
                y: Int
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let errors = upgrade_subgraphs_if_necessary(vec![s1, s2]).expect_err("should fail");
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].to_string(),
            r#"The @interfaceObject directive is used on type "A" in subgraph "s1", which requires other subgraphs to resolve its type name via an interface @key. However, @key on an interface in a federation 1 subgraph does not mean it can fulfill the __typename-resolution requirement that @interfaceObject depends on. For subgraph "s2", either upgrade them to federation 2 subgraphs or remove @key from the type."#
        );
    }

    #[test]
    fn allows_interface_object_usage_when_no_fed1_subgraph_has_interface_key() {
        // When @interfaceObject is used in a fed2 subgraph but no fed1 subgraph has @key on the
        // corresponding interface type, there is no conflict and composition should succeed.
        let s1 = Subgraph::parse("s1", "", r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.3", import: [ "@key", "@interfaceObject"])

            type Query {
                a: A
            }

            type A @key(fields: "id") @interfaceObject {
                id: String
                x: Int
            }
        "#)
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        // s2 is a fed1 subgraph but has @key only on object type B, not on any interface named A.
        let s2 = Subgraph::parse(
            "s2",
            "",
            r#"
            type B @key(fields: "id") {
                id: String
                y: Int
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        // Should NOT error: @interfaceObject on "A" in s1 has no conflicting fed1 interface @key.
        upgrade_subgraphs_if_necessary(vec![s1, s2]).expect("should succeed");
    }

    #[test]
    fn allows_interface_object_usage_when_all_subgraphs_are_fed2() {
        // When all subgraphs are fed2, the upgrader is never invoked, so @interfaceObject
        // combined with @key on an interface in another fed2 subgraph is perfectly valid.
        let s1 = Subgraph::parse("s1", "", r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.3", import: [ "@key", "@interfaceObject"])

            type Query {
                a: A
            }

            type A @key(fields: "id") @interfaceObject {
                id: String
                x: Int
            }
        "#)
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        // s2 is also a fed2 subgraph with @key on interface A.
        let s2 = Subgraph::parse(
            "s2",
            "",
            r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.3", import: [ "@key"])

            interface A @key(fields: "id") {
                id: String
                y: Int
            }

            type X implements A @key(fields: "id") {
                id: String
                y: Int
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        // Should NOT error: all subgraphs are fed2, no upgrader involved.
        upgrade_subgraphs_if_necessary(vec![s1, s2]).expect("should succeed");
    }

    #[test]
    fn allows_fed1_interface_key_without_interface_object_in_any_fed2_subgraph() {
        // A fed1 subgraph with @key on an interface is fine on its own — the key is simply
        // stripped during upgrade. No error should be raised when no fed2 subgraph uses
        // @interfaceObject on that type.
        let s1 = Subgraph::parse("s1", "", r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.3", import: [ "@key", "@interfaceObject"])

            type Query {
                b: B
            }

            type B @key(fields: "id") @interfaceObject {
                id: String
                x: Int
            }
        "#)
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        // s2 is a fed1 subgraph with @key on interface A — but no fed2 subgraph uses
        // @interfaceObject on type A.
        let s2 = Subgraph::parse(
            "s2",
            "",
            r#"
            interface A @key(fields: "id") {
                id: String
                y: Int
            }

            type X implements A @key(fields: "id") {
                id: String
                y: Int
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        // Should NOT error: no fed2 subgraph uses @interfaceObject on type A.
        upgrade_subgraphs_if_necessary(vec![s1, s2]).expect("should succeed");
    }

    #[test]
    fn reports_multiple_errors_for_multiple_conflicting_types() {
        // When a fed2 subgraph uses @interfaceObject on two different types (A and B), and a
        // fed1 subgraph has @key on both interface A and interface B, two separate errors must be
        // emitted — one per conflicting type.
        let s1 = Subgraph::parse("s1", "", r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.3", import: [ "@key", "@interfaceObject"])

            type Query {
                a: A
            }

            type A @key(fields: "id") @interfaceObject {
                id: String
            }

            type B @key(fields: "id") @interfaceObject {
                id: String
            }
        "#)
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let s2 = Subgraph::parse(
            "s2",
            "",
            r#"
            interface A @key(fields: "id") {
                id: String
            }

            type XA implements A @key(fields: "id") {
                id: String
            }

            interface B @key(fields: "id") {
                id: String
            }

            type XB implements B @key(fields: "id") {
                id: String
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let errors = inner_upgrade_subgraphs_if_necessary(vec![s1, s2]).expect_err("should fail");
        assert_eq!(errors.len(), 2);
        assert!(
            errors.iter().any(|e| e.to_string().contains(r#"type "A""#)),
            "expected error mentioning type A"
        );
        assert!(
            errors.iter().any(|e| e.to_string().contains(r#"type "B""#)),
            "expected error mentioning type B"
        );
    }

    #[test]
    fn error_message_includes_all_fed2_subgraphs_using_interface_object_on_same_type() {
        // When multiple fed2 subgraphs all use @interfaceObject on the same type, all their names
        // must appear in the error message.
        let s1 = Subgraph::parse("s1", "", r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.3", import: [ "@key", "@interfaceObject"])

            type Query {
                a: A
            }

            type A @key(fields: "id") @interfaceObject {
                id: String
                x: Int
            }
        "#)
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let s2 = Subgraph::parse("s2", "", r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.3", import: [ "@key", "@interfaceObject"])

            type A @key(fields: "id") @interfaceObject {
                id: String
                y: Int
            }
        "#)
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let s3 = Subgraph::parse(
            "s3",
            "",
            r#"
            interface A @key(fields: "id") {
                id: String
            }

            type X implements A @key(fields: "id") {
                id: String
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let errors =
            inner_upgrade_subgraphs_if_necessary(vec![s1, s2, s3]).expect_err("should fail");
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].to_string(),
            r#"The @interfaceObject directive is used on type "A" in subgraphs "s1" and "s2", which requires other subgraphs to resolve its type name via an interface @key. However, @key on an interface in a federation 1 subgraph does not mean it can fulfill the __typename-resolution requirement that @interfaceObject depends on. For subgraph "s3", either upgrade them to federation 2 subgraphs or remove @key from the type."#
        );
    }

    #[test]
    fn error_message_includes_all_fed1_subgraphs_with_interface_key_on_same_type() {
        // When multiple fed1 subgraphs all have @key on the same interface type, all their names
        // must appear in the error message (the "For ..." part).
        let s1 = Subgraph::parse("s1", "", r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.3", import: [ "@key", "@interfaceObject"])

            type Query {
                a: A
            }

            type A @key(fields: "id") @interfaceObject {
                id: String
                x: Int
            }
        "#)
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let s2 = Subgraph::parse(
            "s2",
            "",
            r#"
            interface A @key(fields: "id") {
                id: String
            }

            type X implements A @key(fields: "id") {
                id: String
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let s3 = Subgraph::parse(
            "s3",
            "",
            r#"
            interface A @key(fields: "id") {
                id: String
            }

            type Y implements A @key(fields: "id") {
                id: String
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let errors =
            inner_upgrade_subgraphs_if_necessary(vec![s1, s2, s3]).expect_err("should fail");
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].to_string(),
            r#"The @interfaceObject directive is used on type "A" in subgraph "s1", which requires other subgraphs to resolve its type name via an interface @key. However, @key on an interface in a federation 1 subgraph does not mean it can fulfill the __typename-resolution requirement that @interfaceObject depends on. For subgraphs "s2" and "s3", either upgrade them to federation 2 subgraphs or remove @key from the type."#
        );
    }

    #[test]
    fn handles_addition_of_shareable_when_external_is_used_on_type() {
        let s1 = Subgraph::parse(
            "s1",
            "",
            r#"
            type Query {
                t1: T
            }

            type T @key(fields: "id") {
                id: String
                x: Int
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let s2 = Subgraph::parse(
            "s2",
            "",
            r#"
            type Query {
                t2: T
            }

            type T @external {
                x: Int
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let [s1, s2]: [Subgraph<_>; 2] = upgrade_subgraphs_if_necessary(vec![s1, s2])
            .expect("upgrades schema")
            .try_into()
            .expect("Expected 2 elements");

        // 2 things must happen here:
        // 1. the @external on type `T` in s2 should be removed, as @external on types were no-ops in fed1 (but not in fed2 anymore, hence the removal)
        // 2. field `T.x` in s1 must be marked @shareable since it is resolved by s2 (since again, it's @external annotation is ignored).
        assert!(
            s2.schema()
                .schema()
                .types
                .get("T")
                .is_some_and(|t| !t.directives().has("external"))
        );
        assert!(
            s1.schema()
                .schema()
                .type_field("T", "x")
                .is_ok_and(|f| f.directives.has("shareable"))
        );
    }

    #[test]
    fn fully_upgrades_schema_with_no_link_directives() {
        let subgraph = Subgraph::parse(
            "subgraph",
            "",
            r#"
            type Query {
                hello: String
            }
            "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let [subgraph]: [Subgraph<_>; 1] = upgrade_subgraphs_if_necessary(vec![subgraph])
            .expect("upgrades schema")
            .try_into()
            .expect("Expected 1 element");

        // fed 1 schemas are auto upgraded to fed v2.4
        insta::assert_snapshot!(
            subgraph.schema_string(),
            @r###"
        schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.4", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject"]) {
          query: Query
        }

        directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

        directive @requires(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @provides(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @external(reason: String) on OBJECT | FIELD_DEFINITION

        directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

        directive @extends on OBJECT | INTERFACE

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @shareable repeatable on OBJECT | FIELD_DEFINITION

        directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

        directive @override(from: String!) on FIELD_DEFINITION

        directive @composeDirective(name: String) repeatable on SCHEMA

        directive @interfaceObject on OBJECT

        type Query {
          hello: String
          _service: _Service!
        }

        type _Service {
          sdl: String
        }

        scalar _Any

        scalar federation__FieldSet

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

        scalar link__Import
        "###
        );
    }

    #[test]
    fn does_not_add_shareable_to_subscriptions() {
        let subgraph1 = Subgraph::parse(
            "subgraph1",
            "",
            r#"
            type Query {
                hello: String
            }

            type Subscription {
                update: String!
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let subgraph2 = Subgraph::parse(
            "subgraph2",
            "",
            r#"
            type Query {
                hello: String
            }

            type Subscription {
                update: String!
            }
            "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let [subgraph1, subgraph2]: [Subgraph<_>; 2] =
            upgrade_subgraphs_if_necessary(vec![subgraph1, subgraph2])
                .expect("upgrades schema")
                .try_into()
                .expect("Expected 2 elements");
        assert!(
            !subgraph1
                .schema()
                .schema()
                .to_string()
                .contains("update: String! @shareable")
        );
        assert!(
            !subgraph2
                .schema()
                .schema()
                .to_string()
                .contains("update: String! @shareable")
        );
        assert!(
            subgraph1
                .schema()
                .schema()
                .types
                .get("Subscription")
                .is_some_and(|s| !s.directives().has("shareable"))
        );
        assert!(
            subgraph2
                .schema()
                .schema()
                .types
                .get("Subscription")
                .is_some_and(|s| !s.directives().has("shareable"))
        );
    }

    #[test]
    fn reject_incompatible_redeclared_link_directive() {
        let subgraph1 = Subgraph::parse(
            "subgraph1",
            "",
            r#"
                extend schema
                    @link(url: "https://specs.apollo.dev/link/v1.0", as: "link")

                directive @link(url: String!, as: String) repeatable on SCHEMA

                type Query {
                    test: Int!
                }
            "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        upgrade_subgraphs_if_necessary(vec![subgraph1])
            .expect_err("Error: the argument `import` is not supported by `@link`");
    }

    #[test]
    fn handle_compatible_renamed_link_directive() {
        let subgraph1 = Subgraph::parse(
            "subgraph1",
            "",
            r#"
                extend schema
                    @linkX(url: "https://specs.apollo.dev/link/v1.0", as: "linkX")

                directive @linkX(url: String!, as: String, import: [String]) repeatable on SCHEMA

                type Query {
                    test: Int!
                }
            "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let [s1]: [Subgraph<_>; 1] = upgrade_subgraphs_if_necessary(vec![subgraph1])
            .expect("successfully upgraded")
            .try_into()
            .expect("single graph");
        insta::assert_snapshot!(
            s1.schema_string(),
            @r###"
                schema @linkX(url: "https://specs.apollo.dev/federation/v2.4", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject"]) {
                  query: Query
                }

                extend schema @linkX(url: "https://specs.apollo.dev/link/v1.0", as: "linkX")

                directive @linkX(url: String!, as: String, import: [String]) repeatable on SCHEMA

                directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

                directive @requires(fields: federation__FieldSet!) on FIELD_DEFINITION

                directive @provides(fields: federation__FieldSet!) on FIELD_DEFINITION

                directive @external(reason: String) on OBJECT | FIELD_DEFINITION

                directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

                directive @extends on OBJECT | INTERFACE

                directive @shareable repeatable on OBJECT | FIELD_DEFINITION

                directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

                directive @override(from: String!) on FIELD_DEFINITION

                directive @composeDirective(name: String) repeatable on SCHEMA

                directive @interfaceObject on OBJECT

                type Query {
                  test: Int!
                  _service: _Service!
                }

                type _Service {
                  sdl: String
                }

                scalar _Any

                scalar federation__FieldSet
            "###);
    }

    #[test]
    fn handle_implicit_core_directive_alias() {
        // This used to panic.
        // The stray `@core` directive definition is not used on `schema` definition, thus it's not
        // recognized as a link directive.  causes the initial expansion to alias the link
        // (@core) directive. Make sure we can handle that aliasing correctly.
        let subgraph1 = Subgraph::parse(
            "subgraph1",
            "",
            r#"
                directive @core(feature: String!) repeatable on SCHEMA

                type Query {
                    test: Int!
                }
            "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        // @core is not applied on the schema so it is treated as regular custom directive
        let [upgraded] = upgrade_subgraphs_if_necessary(vec![subgraph1])
            .expect("upgrades schema")
            .try_into()
            .expect("single subgraph was upgraded");
        insta::assert_snapshot!(
            upgraded.schema_string(),
            @r###"
                schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.4", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject"]) {
                  query: Query
                }

                directive @core(feature: String!) repeatable on SCHEMA

                directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

                directive @requires(fields: federation__FieldSet!) on FIELD_DEFINITION

                directive @provides(fields: federation__FieldSet!) on FIELD_DEFINITION

                directive @external(reason: String) on OBJECT | FIELD_DEFINITION

                directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

                directive @extends on OBJECT | INTERFACE

                directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

                directive @shareable repeatable on OBJECT | FIELD_DEFINITION

                directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

                directive @override(from: String!) on FIELD_DEFINITION

                directive @composeDirective(name: String) repeatable on SCHEMA

                directive @interfaceObject on OBJECT

                type Query {
                  test: Int!
                  _service: _Service!
                }

                type _Service {
                  sdl: String
                }

                scalar _Any

                scalar federation__FieldSet

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

                scalar link__Import
            "###);
    }

    #[test]
    fn handle_link_directive_name_conflict() {
        // This used to panic.
        // The stray `@link` directive definition in the original subgraph is not used on `schema`
        // definition, thus it's not recognized as a link directive. Since it's not recognized as a
        // link directive, it will stay in the upgraded schema. Thus, the upgrader must alias the
        // new link directive.
        let subgraph1 = Subgraph::parse(
            "subgraph1",
            "",
            r#"
                directive @link(url: String!) repeatable on SCHEMA

                type Query {
                    test: Int!
                }
            "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let [upgraded] = upgrade_subgraphs_if_necessary(vec![subgraph1])
            .expect("upgrades schema")
            .try_into()
            .expect("single subgraph was upgraded");
        // keeps user provided @link and adds federation directive aliased to link1
        insta::assert_snapshot!(
            upgraded.schema_string(),
            @r###"
                schema @link1(url: "https://specs.apollo.dev/link/v1.0", as: "link1") @link1(url: "https://specs.apollo.dev/federation/v2.4", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject"]) {
                  query: Query
                }

                directive @link(url: String!) repeatable on SCHEMA

                directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

                directive @requires(fields: federation__FieldSet!) on FIELD_DEFINITION

                directive @provides(fields: federation__FieldSet!) on FIELD_DEFINITION

                directive @external(reason: String) on OBJECT | FIELD_DEFINITION

                directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

                directive @extends on OBJECT | INTERFACE

                directive @link1(url: String, as: String, for: link1__Purpose, import: [link1__Import]) repeatable on SCHEMA

                directive @shareable repeatable on OBJECT | FIELD_DEFINITION

                directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

                directive @override(from: String!) on FIELD_DEFINITION

                directive @composeDirective(name: String) repeatable on SCHEMA

                directive @interfaceObject on OBJECT

                type Query {
                  test: Int!
                  _service: _Service!
                }

                type _Service {
                  sdl: String
                }

                scalar _Any

                scalar federation__FieldSet

                enum link1__Purpose {
                  """
                  `SECURITY` features provide metadata necessary to securely resolve fields.
                  """
                  SECURITY
                  """
                  `EXECUTION` features provide metadata necessary for operation execution.
                  """
                  EXECUTION
                }

                scalar link1__Import
            "###);
    }

    #[test]
    fn removes_provides_on_non_composite_fields() {
        let subgraphs = vec![
            (
                "subgraph1",
                r#"
                type User @key(fields: "id") {
                    id: ID! @external @provides(fields: "id")
                    a: Int!
                }
            "#,
            ),
            (
                "subgraph2",
                r#"
                type User @key(fields: "id") {
                    id: ID!
                    b: Int!
                }

                type Query {
                    start: User
                }
            "#,
            ),
        ];

        let expanded = subgraphs
            .into_iter()
            .map(|(name, schema)| {
                Subgraph::parse(name, "", schema)
                    .expect("parses schema")
                    .expand_links()
                    .expect("expands schema")
            })
            .collect::<Vec<_>>();
        let upgraded = upgrade_subgraphs_if_necessary(expanded).expect("failed to upgrade schema");
        let changed_schema = upgraded[0].schema().schema();
        let id_field = coord!(User.id)
            .lookup_field(changed_schema)
            .expect("field exists");
        assert!(
            id_field.directives.get("provides").is_none(),
            "User.id in subgraph1 should not have @provides"
        );
    }

    #[test]
    fn removes_type_from_upgraded_schema_when_all_fields_unused() {
        let subgraph1 = Subgraph::parse(
            "subgraph1",
            "",
            r#"
                type T {
                  a: Int! @external
                }
            "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        // Type T should still be in the expanded schema
        assert!(subgraph1.schema().schema.get_object("T").is_some());
        insta::assert_snapshot!(
            subgraph1.schema_string(),
            @r###"
        directive @key(fields: _FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

        directive @requires(fields: _FieldSet!) on FIELD_DEFINITION

        directive @provides(fields: _FieldSet!) on FIELD_DEFINITION

        directive @external(reason: String) on OBJECT | FIELD_DEFINITION

        directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

        directive @extends on OBJECT | INTERFACE

        type T {
          a: Int! @external
        }

        scalar _FieldSet

        scalar _Any

        type _Service {
          sdl: String
        }

        type Query {
          _service: _Service!
        }
        "###);

        let [s1] = upgrade_subgraphs_if_necessary(vec![subgraph1])
            .expect("upgrades schema")
            .try_into()
            .expect("Expected 1 element");

        // Type T should NOT be in the upgraded schema
        assert!(!s1.schema_string().contains("type T"));
        insta::assert_snapshot!(
            s1.schema_string(),
            @r###"
        schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.4", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject"]) {
          query: Query
        }

        directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

        directive @requires(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @provides(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @external(reason: String) on OBJECT | FIELD_DEFINITION

        directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

        directive @extends on OBJECT | INTERFACE

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @shareable repeatable on OBJECT | FIELD_DEFINITION

        directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

        directive @override(from: String!) on FIELD_DEFINITION

        directive @composeDirective(name: String) repeatable on SCHEMA

        directive @interfaceObject on OBJECT

        type Query {
          _service: _Service!
        }

        scalar _Any

        type _Service {
          sdl: String
        }

        scalar federation__FieldSet

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

        scalar link__Import
        "###);
    }

    #[test]
    fn removes_external_from_composite_key_fields_in_extension_without_key() {
        let subgraph1 = Subgraph::parse(
            "subgraph1",
            "",
            r#"
            type Query {
                items: [Item!]!
            }

            type Item @key(fields: "id sku") {
                id: ID!
                sku: String!
                name: String
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let subgraph2 = Subgraph::parse(
            "subgraph2",
            "",
            r#"
            # Extension without @key - fields should have @external removed
            # because they're part of the key in subgraph1
            extend type Item {
                id: ID! @external
                sku: String! @external
                price: Float
            }
        "#,
        )
        .expect("parses schema")
        .expand_links()
        .expect("expands schema");

        let [_s1, s2]: [Subgraph<_>; 2] =
            upgrade_subgraphs_if_necessary(vec![subgraph1, subgraph2])
                .expect("upgrades schema")
                .try_into()
                .expect("Expected 2 elements");

        // Verify that subgraph2's Item type was upgraded successfully
        // and doesn't have @external on the key fields
        let s2_schema = s2.schema().schema();

        let id_field = coord!(Item.id)
            .lookup_field(s2_schema)
            .expect("id field exists");
        assert!(
            !id_field.directives.has("external"),
            "id should not have @external"
        );

        let sku_field = coord!(Item.sku)
            .lookup_field(s2_schema)
            .expect("sku field exists");
        assert!(
            !sku_field.directives.has("external"),
            "sku should not have @external"
        );

        let price_field = coord!(Item.price)
            .lookup_field(s2_schema)
            .expect("price field exists");
        assert!(
            !price_field.directives.has("external"),
            "price should not have @external"
        );
    }

    #[test]
    fn keeps_user_provided_directive_definitions() {
        let sdl = r#"
schema {
  query: Query
}

"Marks the field, argument, input field or enum value as deprecated"
directive @deprecated(
    "The reason for the deprecation"
    reason: String = "No longer supported"
  ) on FIELD_DEFINITION | ARGUMENT_DEFINITION | ENUM_VALUE | INPUT_FIELD_DEFINITION

"Marks target object as extending part of the federated schema"
directive @extends on OBJECT | INTERFACE

"Marks target field as external meaning it will be resolved by federated schema"
directive @external on FIELD_DEFINITION

"Directs the executor to include this field or fragment only when the `if` argument is true"
directive @include(
    "Included when true."
    if: Boolean!
  ) on FIELD | FRAGMENT_SPREAD | INLINE_FRAGMENT

"This is a custom definition for key"
directive @key(fields: _FieldSet!) repeatable on OBJECT | INTERFACE

"This is a custom definition for provides"
directive @provides(fields: _FieldSet!) on FIELD_DEFINITION

"This is a custom definition for requires"
directive @requires(fields: _FieldSet!) on FIELD_DEFINITION

"Directs the executor to skip this field or fragment when the `if`'argument is true."
directive @skip(
    "Skipped when true."
    if: Boolean!
  ) on FIELD | FRAGMENT_SPREAD | INLINE_FRAGMENT

"Exposes a URL that specifies the behaviour of this scalar."
directive @specifiedBy(
    "The URL that specifies the behaviour of this scalar."
    url: String!
  ) on SCALAR

"Allows users to annotate fields and types with additional metadata information"
directive @tag(name: String!) repeatable on SCALAR | OBJECT | FIELD_DEFINITION | ARGUMENT_DEFINITION | INTERFACE | UNION | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

union _Entity = T

type Query @extends {
  "Union of all types that use the @key directive, including both types native to the schema and extended types"
  _entities(representations: [_Any!]!): [_Entity]!
  _service: _Service!
  t(id: ID!): T
}

type _Service {
  sdl: String!
}

type T @key(fields: "id") {
  id: ID!
  name: String
}

"Federation scalar type used to represent any external entities passed to _entities query."
scalar _Any

"Federation type representing set of fields"
scalar _FieldSet
"#;

        let s1 = Subgraph::parse("s1", "http://s1", sdl)
            .expect("valid subgraph")
            .expand_links()
            .expect("expanded");
        insta::assert_snapshot!(
            s1.schema_string(),
            @r###"
                """
                Directs the executor to skip this field or fragment when the `if`'argument is true.
                """
                directive @skip(
                  """Skipped when true."""
                  if: Boolean!,
                ) on FIELD | FRAGMENT_SPREAD | INLINE_FRAGMENT

                """
                Directs the executor to include this field or fragment only when the `if` argument is true
                """
                directive @include(
                  """Included when true."""
                  if: Boolean!,
                ) on FIELD | FRAGMENT_SPREAD | INLINE_FRAGMENT

                """Marks the field, argument, input field or enum value as deprecated"""
                directive @deprecated(
                  """The reason for the deprecation"""
                  reason: String = "No longer supported",
                ) on FIELD_DEFINITION | ARGUMENT_DEFINITION | ENUM_VALUE | INPUT_FIELD_DEFINITION

                """Exposes a URL that specifies the behaviour of this scalar."""
                directive @specifiedBy(
                  """The URL that specifies the behaviour of this scalar."""
                  url: String!,
                ) on SCALAR

                """Marks target object as extending part of the federated schema"""
                directive @extends on OBJECT | INTERFACE

                """
                Marks target field as external meaning it will be resolved by federated schema
                """
                directive @external on FIELD_DEFINITION

                """This is a custom definition for key"""
                directive @key(fields: _FieldSet!) repeatable on OBJECT | INTERFACE

                """This is a custom definition for provides"""
                directive @provides(fields: _FieldSet!) on FIELD_DEFINITION

                """This is a custom definition for requires"""
                directive @requires(fields: _FieldSet!) on FIELD_DEFINITION

                """
                Allows users to annotate fields and types with additional metadata information
                """
                directive @tag(name: String!) repeatable on SCALAR | OBJECT | FIELD_DEFINITION | ARGUMENT_DEFINITION | INTERFACE | UNION | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

                union _Entity = T

                type Query @extends {
                  """
                  Union of all types that use the @key directive, including both types native to the schema and extended types
                  """
                  _entities(representations: [_Any!]!): [_Entity]!
                  _service: _Service!
                  t(id: ID!): T
                }

                type _Service {
                  sdl: String!
                }

                type T @key(fields: "id") {
                  id: ID!
                  name: String
                }

                """
                Federation scalar type used to represent any external entities passed to _entities query.
                """
                scalar _Any

                """Federation type representing set of fields"""
                scalar _FieldSet
            "###
        );

        let [upgraded]: [Subgraph<_>; 1] = upgrade_subgraphs_if_necessary(vec![s1])
            .expect("success")
            .try_into()
            .expect("single subgraph upgraded");
        insta::assert_snapshot!(
            upgraded.schema_string(),
            @r###"
                schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.4", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject"]) {
                  query: Query
                }

                """
                Directs the executor to skip this field or fragment when the `if`'argument is true.
                """
                directive @skip(
                  """Skipped when true."""
                  if: Boolean!,
                ) on FIELD | FRAGMENT_SPREAD | INLINE_FRAGMENT

                """
                Directs the executor to include this field or fragment only when the `if` argument is true
                """
                directive @include(
                  """Included when true."""
                  if: Boolean!,
                ) on FIELD | FRAGMENT_SPREAD | INLINE_FRAGMENT

                """Marks the field, argument, input field or enum value as deprecated"""
                directive @deprecated(
                  """The reason for the deprecation"""
                  reason: String = "No longer supported",
                ) on FIELD_DEFINITION | ARGUMENT_DEFINITION | ENUM_VALUE | INPUT_FIELD_DEFINITION

                """Exposes a URL that specifies the behaviour of this scalar."""
                directive @specifiedBy(
                  """The URL that specifies the behaviour of this scalar."""
                  url: String!,
                ) on SCALAR

                """Marks target object as extending part of the federated schema"""
                directive @extends on OBJECT | INTERFACE

                """
                Marks target field as external meaning it will be resolved by federated schema
                """
                directive @external on FIELD_DEFINITION

                """This is a custom definition for key"""
                directive @key(fields: federation__FieldSet!) repeatable on OBJECT | INTERFACE

                """This is a custom definition for provides"""
                directive @provides(fields: federation__FieldSet!) on FIELD_DEFINITION

                """This is a custom definition for requires"""
                directive @requires(fields: federation__FieldSet!) on FIELD_DEFINITION

                """
                Allows users to annotate fields and types with additional metadata information
                """
                directive @tag(name: String!) repeatable on SCALAR | OBJECT | FIELD_DEFINITION | ARGUMENT_DEFINITION | INTERFACE | UNION | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

                directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

                directive @shareable repeatable on OBJECT | FIELD_DEFINITION

                directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

                directive @override(from: String!) on FIELD_DEFINITION

                directive @composeDirective(name: String) repeatable on SCHEMA

                directive @interfaceObject on OBJECT

                union _Entity = T

                type Query @extends {
                  """
                  Union of all types that use the @key directive, including both types native to the schema and extended types
                  """
                  _entities(representations: [_Any!]!): [_Entity]!
                  _service: _Service!
                  t(id: ID!): T
                }

                type _Service {
                  sdl: String!
                }

                type T @key(fields: "id") {
                  id: ID!
                  name: String
                }

                """
                Federation scalar type used to represent any external entities passed to _entities query.
                """
                scalar _Any

                """Federation type representing set of fields"""
                scalar federation__FieldSet

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

                scalar link__Import
            "###
        );
    }
}
