use std::collections::HashSet;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Value;
use apollo_compiler::collections::HashMap;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;

use super::compute_subgraph_metadata;
use super::FederationSchema;
use super::TypeDefinitionPosition;
use super::field_set::collect_target_fields_from_field_set;
use super::position::FieldDefinitionPosition;
use super::position::InterfaceFieldDefinitionPosition;
use super::position::InterfaceTypeDefinitionPosition;
use super::position::ObjectTypeDefinitionPosition;
use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::internal_error;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::schema::SubgraphMetadata;
use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceTypeDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionKind;
use crate::subgraph::typestate::Expanded;
use crate::subgraph::typestate::Raw;
use crate::subgraph::typestate::Subgraph;
use crate::supergraph::remove_inactive_requires_and_provides_from_subgraph;
use crate::utils::FallibleIterator;

#[derive(Debug)]
struct SchemaUpgrader<'a> {
    schema: FederationSchema,
    expanded_info: ExpandedSubgraphInfo,
    subgraphs: &'a IndexMap<String, Subgraph<Expanded>>,
    object_type_map: &'a HashMap<Name, HashMap<String, TypeInfo>>,
}

#[derive(Clone, Debug)]
#[allow(unused)]
struct TypeInfo {
    pos: TypeDefinitionPosition,
    metadata: SubgraphMetadata,
}

#[derive(Debug)]
struct ExpandedSubgraphInfo {
    subgraph_name: String,
    key_directive_name: Option<Name>,
    requires_directive_name: Option<Name>,
    provides_directive_name: Option<Name>,
    extends_directive_name: Option<Name>,
}

#[allow(unused)]
// PORT_NOTE: In JS, this returns upgraded subgraphs along with a set of messages about what changed.
// However, those messages were never used, so we have omitted them here.
pub(crate) fn upgrade_subgraphs_if_necessary(
    subgraphs: Vec<Subgraph<Expanded>>,
) -> Result<Vec<Subgraph<Expanded>>, FederationError> {
    // if all subgraphs are fed 2, there is no upgrade to be done
    if subgraphs.iter().all(|subgraph| !subgraph.is_fed_1()) {
        return Ok(subgraphs);
    }

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
    
    // insertion order is preserved with IndexMap
    let subgraphs: IndexMap<String, Subgraph<Expanded>> = subgraphs.into_iter().map(|subgraph| (subgraph.name.clone(), subgraph)).collect();
    
    let mut upgraded: HashMap<String, Subgraph<Expanded>> = Default::default();
    for (name, subgraph) in subgraphs.iter() {
        if subgraph.is_fed_1() {
            let mut upgrader = SchemaUpgrader::new(subgraph, &subgraphs, &object_type_map)?;
            upgrader.upgrade()?;
            let new_subgraph = Subgraph::<Raw>::new(
                subgraph.name.as_str(),
                subgraph.url.as_str(),
                upgrader.schema.schema().clone(),
            )
            .assume_expanded()?;
            upgraded.insert(subgraph.name.clone(), new_subgraph);
        }
    }
    Ok(subgraphs.into_iter().map(|(name, subgraph)| {
        if subgraph.is_fed_1() {
            return upgraded.remove(&name).unwrap();
        }
        subgraph
    }).collect())
}

// Extensions for FederationError to provide additional error types
impl FederationError {
    pub fn extension_with_no_base(message: &str) -> Self {
        // Fixed: use internal() instead of new()
        FederationError::internal(format!("EXTENSION_WITH_NO_BASE: {}", message))
    }
}

impl<'a> SchemaUpgrader<'a> {
    #[allow(unused)]
    fn new(
        original_subgraph: &'a Subgraph<Expanded>,
        subgraphs: &'a IndexMap<String, Subgraph<Expanded>>,
        object_type_map: &'a HashMap<Name, HashMap<String, TypeInfo>>,
    ) -> Result<Self, FederationError> {
        let schema = original_subgraph.schema().clone();
        Ok(SchemaUpgrader {
            schema,
            expanded_info: ExpandedSubgraphInfo {
                subgraph_name: original_subgraph.name.clone(),
                key_directive_name: original_subgraph.key_directive_name()?.clone(),
                requires_directive_name: original_subgraph.requires_directive_name()?.clone(),
                provides_directive_name: original_subgraph.provides_directive_name()?.clone(),
                extends_directive_name: original_subgraph.extends_directive_name()?.clone(),     
            },
            subgraphs,
            object_type_map,
        })
    }
    
    // because the schema may have been changed since the last time metadata was calculated, we need to create it every time it's needed.
    fn subgraph_metadata(&self) -> Result<SubgraphMetadata, FederationError> {
        compute_subgraph_metadata(&self.schema)?.ok_or_else(|| {
            internal_error!(
                "Unable to detect federation version used in subgraph '{}'",
                self.expanded_info.subgraph_name
            )
        })
    }
    
    // function to get subgraph from list of subgraphs by name. Right now it will just iterate, but perhaps the struct should be a HashMap eventually
    fn get_subgraph_by_name(&self, name: &String) -> Option<&Subgraph<Expanded>> {
        self.subgraphs.get(name)
    }

    #[allow(unused)]
    fn upgrade(&mut self) -> Result<(), FederationError> {
        // Run pre-upgrade validations to check for issues that would prevent upgrade
        self.pre_upgrade_validations()?;

        // Fix federation directive arguments (fields) to ensure they're proper strings
        // Note: Implementation simplified for compilation purposes
        self.fix_federation_directives_arguments()?;

        self.remove_external_on_interface();

        self.remove_external_on_object_types();

        // Note that we remove all external on type extensions first, so we don't have to care about it later in @key, @provides and @requires.
        self.remove_external_on_type_extensions()?;

        self.fix_inactive_provides_and_requires();

        self.remove_type_extensions();

        self.remove_directives_on_interface();

        // Note that this rule rely on being after `removeDirectivesOnInterface` in practice (in that it doesn't check interfaces).
        self.remove_provides_on_non_composite();

        // Note that this should come _after_ all the other changes that may remove/update federation directives, since those may create unused
        // externals. Which is why this is toward  the end.
        self.remove_unused_externals();

        self.add_shareable()?;

        self.remove_tag_on_external()?;

        Ok(())
    }

    // integrates checkForExtensionWithNoBase from the JS code
    fn pre_upgrade_validations(&self) -> Result<(), FederationError> {
        let schema = &self.schema;

        // Iterate through all types and check if they're federation type extensions without a base
        for type_pos in schema.get_types() {
            if self.is_root_type_extension(&type_pos)
                || !self.is_federation_type_extension(&type_pos)?
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
                            subgraph_name.as_str() != self.expanded_info.subgraph_name.as_str()
                        })
                        .fallible_any(|(other_name, type_info)| {
                            let Some(other_subgraph) = self.get_subgraph_by_name(other_name)
                            else {
                                return Ok(false);
                            };
                            let extended_type =
                                type_info.pos.get(other_subgraph.schema().schema())?;
                            Ok::<bool, FederationError>(Self::has_non_extension_elements(
                                extended_type,
                            ))
                        })
                })
                .unwrap_or(Ok(false))?;

            if !has_non_extension_definition {
                return Err(FederationError::extension_with_no_base(&format!(
                    "Type \"{}\" is an extension type, but there is no type definition for \"{}\" in any subgraph.",
                    type_name, type_name
                )));
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

    fn fix_federation_directives_arguments(&mut self) -> Result<(), FederationError> {
        let schema = &mut self.schema;

        // both @provides and @requires will only have an object_fields referencer
        for directive_name in ["requires", "provides"] {
            let referencers = schema.referencers().get_directive(directive_name)?;
            for field in &referencers.object_fields.clone() {
                let field_type = field.make_mut(&mut schema.schema)?.make_mut();

                for directive in field_type.directives.0.iter_mut() {
                    if directive.name == directive_name {
                        for arg in directive.make_mut().arguments.iter_mut() {
                            if arg.name == "fields" {
                                if let Some(new_fields_string) =
                                    Self::make_fields_string_if_not(&arg.value)?
                                {
                                    *arg.make_mut().value.make_mut() =
                                        Value::String(new_fields_string);
                                }
                                break;
                            }
                        }
                    }
                }
            }
        }

        // now do the exact same thing for @key. The difference is that the directive location will be object_types
        // rather than object_fields
        let referencers = schema.referencers().get_directive("key")?;
        for field in &referencers.object_types.clone() {
            let field_type = field.make_mut(&mut schema.schema)?.make_mut();

            for directive in field_type.directives.0.iter_mut() {
                if directive.name == "key" {
                    for arg in directive.make_mut().arguments.iter_mut() {
                        if arg.name == "fields" {
                            if let Some(new_fields_string) =
                                Self::make_fields_string_if_not(&arg.value)?
                            {
                                *arg.make_mut().value.make_mut() = Value::String(new_fields_string);
                            }
                            break;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn remove_external_on_interface(&mut self) -> Result<(), FederationError> {
        let schema = &mut self.schema;
        let Some(metadata) = &schema.subgraph_metadata else {
            return Ok(());
        };
        let Ok(external_directive) = metadata
            .federation_spec_definition()
            .external_directive_definition(schema)
        else {
            return Ok(());
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
        }
        Ok(())
    }

    fn remove_external_on_object_types(&mut self) -> Result<(), FederationError> {
        let schema = &mut self.schema;
        let Some(metadata) = &schema.subgraph_metadata else {
            return Ok(());
        };
        let Ok(external_directive) = metadata
            .federation_spec_definition()
            .external_directive_definition(schema)
        else {
            return Ok(());
        };
        let mut to_delete: Vec<(ObjectTypeDefinitionPosition, Component<Directive>)> = vec![];
        for (obj_name, ty) in &schema.schema().types {
            let ExtendedType::Object(_) = ty else {
                continue;
            };

            let object_pos = ObjectTypeDefinitionPosition::new(obj_name.clone());
            let directives = object_pos.get_applied_directives(schema, &external_directive.name);
            if !directives.is_empty() {
                to_delete.push((object_pos, directives[0].clone()));
            }
        }
        for (pos, directive) in to_delete {
            pos.remove_directive(schema, &directive);
        }
        Ok(())
    }

    fn remove_external_on_type_extensions(&mut self) -> Result<(), FederationError> {
        let Some(metadata) = &self.schema.subgraph_metadata else {
            return Ok(());
        };
        let types: Vec<_> = self.schema.get_types().collect();
        let key_directive = metadata
            .federation_spec_definition()
            .key_directive_definition(&self.schema)?;
        let external_directive = metadata
            .federation_spec_definition()
            .external_directive_definition(&self.schema)?;

        let mut to_remove = vec![];
        for ty in &types {
            if !ty.is_composite_type()
                || (!self.is_federation_type_extension(ty)? && !self.is_root_type_extension(ty))
            {
                continue;
            }

            let key_applications = ty.get_applied_directives(&self.schema, &key_directive.name);
            if !key_applications.is_empty() {
                for directive in key_applications {
                    let args = metadata
                        .federation_spec_definition()
                        .key_directive_arguments(directive)?;
                    for field in collect_target_fields_from_field_set(
                        Valid::assume_valid_ref(self.schema.schema()),
                        ty.type_name().clone(),
                        args.fields,
                        false,
                    )? {
                        let external =
                            field.get_applied_directives(&self.schema, &external_directive.name);
                        if !external.is_empty() {
                            to_remove.push((field.clone(), external[0].clone()));
                        }
                    }
                }
            } else {
                // ... but if the extension does _not_ have a key, then if the extension has a field that is
                // part of the _1st_ key on the subgraph owning the type, then this field is not considered
                // external (yes, it's pretty damn random, and it's even worst in that even if the extension
                // does _not_ have the "field of the _1st_ key on the subraph owning the type", then the
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
                    if subgraph_name == self.expanded_info.subgraph_name.as_str() {
                        continue;
                    }
                    let Some(other_schema) = self.get_subgraph_by_name(subgraph_name)
                    else {
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
                    let args = metadata
                        .federation_spec_definition()
                        .key_directive_arguments(directive)?;
                    for field in collect_target_fields_from_field_set(
                        Valid::assume_valid_ref(self.schema.schema()),
                        ty.type_name().clone(),
                        args.fields,
                        false,
                    )? {
                        if TypeDefinitionPosition::from(field.parent()) != info.pos {
                            continue;
                        }
                        let external =
                            field.get_applied_directives(&self.schema, &external_directive.name);
                        if !external.is_empty() {
                            to_remove.push((field.clone(), external[0].clone()));
                        }
                    }
                }
            }
        }

        for (pos, directive) in &to_remove {
            pos.remove_directive(&mut self.schema, directive);
        }
        Ok(())
    }

    fn fix_inactive_provides_and_requires(&mut self) -> Result<(), FederationError> {
        let cloned_schema = self.schema.clone();
        remove_inactive_requires_and_provides_from_subgraph(
            &cloned_schema, // TODO: I don't know what this value should be
            &mut self.schema,
        )
    }

    fn remove_type_extensions(&mut self) -> Result<(), FederationError> {
        let types: Vec<_> = self.schema.get_types().collect();
        for ty in types {
            if !self.is_federation_type_extension(&ty)? && !self.is_root_type_extension(&ty) {
                continue;
            }
            ty.remove_extensions(&mut self.schema)?;
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
    ) -> Result<bool, FederationError> {
        let type_ = ty.get(self.schema.schema())?;
        let has_extend = self
            .expanded_info
            .extends_directive_name
            .as_ref()
            .is_some_and(|extends| type_.directives().has(extends.as_str()));
        Ok((Self::has_extension_elements(type_) || has_extend)
            && (type_.is_object() || type_.is_interface())
            && (has_extend || !Self::has_non_extension_elements(type_)))
    }

    fn has_extension_elements(ty: &ExtendedType) -> bool {
        match ty {
            ExtendedType::Object(obj) => !obj.extensions().is_empty(),
            ExtendedType::Interface(itf) => !itf.extensions().is_empty(),
            ExtendedType::Union(u) => !u.extensions().is_empty(),
            ExtendedType::Enum(e) => !e.extensions().is_empty(),
            ExtendedType::InputObject(io) => !io.extensions().is_empty(),
            ExtendedType::Scalar(s) => !s.extensions().is_empty(),
        }
    }

    fn has_non_extension_elements(ty: &ExtendedType) -> bool {
        ty.directives()
            .iter()
            .any(|d| d.origin.extension_id().is_none())
            || Self::has_non_extension_inner_elements(ty)
    }

    fn has_non_extension_inner_elements(ty: &ExtendedType) -> bool {
        match ty {
            ExtendedType::Scalar(_) => false,
            ExtendedType::Object(t) => {
                t.implements_interfaces
                    .iter()
                    .any(|itf| itf.origin.extension_id().is_none())
                    || t.fields.values().any(|f| f.origin.extension_id().is_none())
            }
            ExtendedType::Interface(t) => {
                t.implements_interfaces
                    .iter()
                    .any(|itf| itf.origin.extension_id().is_none())
                    || t.fields.values().any(|f| f.origin.extension_id().is_none())
            }
            ExtendedType::Union(t) => t.members.iter().any(|m| m.origin.extension_id().is_none()),
            ExtendedType::Enum(t) => t.values.values().any(|v| v.origin.extension_id().is_none()),
            ExtendedType::InputObject(t) => {
                t.fields.values().any(|f| f.origin.extension_id().is_none())
            }
        }
    }

    /// Whether the type is a root type but is declared only as an extension, which federation 1 actually accepts.
    fn is_root_type_extension(&self, pos: &TypeDefinitionPosition) -> bool {
        if !matches!(pos, TypeDefinitionPosition::Object(_))
            || !Self::is_root_type(&self.schema, pos)
        {
            return false;
        }
        let Ok(ty) = pos.get(self.schema.schema()) else {
            return false;
        };
        let has_extends_directive = self
            .expanded_info
            .extends_directive_name
            .as_ref()
            .is_some_and(|extends| ty.directives().has(extends.as_str()));

        has_extends_directive
            || (Self::has_extension_elements(ty) && !Self::has_non_extension_elements(ty))
    }

    fn is_root_type(schema: &FederationSchema, ty: &TypeDefinitionPosition) -> bool {
        schema
            .schema()
            .schema_definition
            .iter_root_operations()
            .any(|op| op.1.as_str() == ty.type_name().as_str())
    }

    fn remove_directives_on_interface(&mut self) -> Result<(), FederationError> {
        if let Some(key) = &self.expanded_info.key_directive_name {
            for pos in &self
                .schema
                .referencers()
                .get_directive(&key)?
                .interface_types
                .clone()
            {
                pos.remove_directive_name(&mut self.schema, &key);

                let fields: Vec<_> = pos.fields(self.schema.schema())?.collect();
                for field in fields {
                    if let Some(provides) = &self.expanded_info.provides_directive_name {
                        field.remove_directive_name(&mut self.schema, &provides);
                    }
                    if let Some(requires) = &self.expanded_info.requires_directive_name {
                        field.remove_directive_name(&mut self.schema, &requires);
                    }
                }
            }
        }

        Ok(())
    }

    fn remove_provides_on_non_composite(&mut self) -> Result<(), FederationError> {
        let schema = &mut self.schema;
        let Some(metadata) = &schema.subgraph_metadata else {
            return Ok(());
        };

        let provides_directive = metadata
            .federation_spec_definition()
            .provides_directive_definition(schema)?;

        #[allow(clippy::iter_overeager_cloned)] // TODO: remove this
        let references_to_remove: Vec<_> = schema
            .referencers()
            .get_directive(provides_directive.name.as_str())?
            .object_fields
            .iter()
            .cloned()
            .filter(|ref_field| {
                schema
                    .get_type(ref_field.type_name.clone())
                    .map(|t| !t.is_composite_type())
                    .unwrap_or(false)
            })
            .collect();
        for reference in &references_to_remove {
            reference.remove(schema)?;
        }
        Ok(())
    }

    fn remove_unused_externals(&mut self) -> Result<(), FederationError> {
        let mut error = MultipleFederationErrors::new();
        let mut fields_to_remove: HashSet<ObjectOrInterfaceFieldDefinitionPosition> =
            HashSet::new();
        let mut types_to_remove: HashSet<ObjectOrInterfaceTypeDefinitionPosition> = HashSet::new();
        for type_ in self.schema.get_types() {
            if let Ok(pos) = ObjectOrInterfaceTypeDefinitionPosition::try_from(type_) {
                let mut has_fields = false;
                for field in pos.fields(self.schema.schema())? {
                    has_fields = true;
                    let field_def = FieldDefinitionPosition::from(field.clone());

                    let metadata = self.subgraph_metadata()?;
                    if metadata.is_field_external(&field_def)
                        && !metadata.is_field_used(&field_def)
                    {
                        fields_to_remove.insert(field);
                    }
                }
                if !has_fields {
                    let is_referenced = match &pos {
                        ObjectOrInterfaceTypeDefinitionPosition::Object(obj_pos) => self
                            .schema
                            .referencers()
                            .object_types
                            .get(&obj_pos.type_name)
                            .is_some_and(|r| r.len() > 0),
                        ObjectOrInterfaceTypeDefinitionPosition::Interface(itf_pos) => self
                            .schema
                            .referencers()
                            .interface_types
                            .get(&itf_pos.type_name)
                            .is_some_and(|r| r.len() > 0),
                    };
                    if is_referenced {
                        error
                            .errors
                            .push(SingleFederationError::TypeWithOnlyUnusedExternal {
                                type_name: pos.type_name().clone(),
                            });
                    } else {
                        types_to_remove.insert(pos);
                    }
                }
            }
        }

        for field in fields_to_remove {
            field.remove(&mut self.schema)?;
        }
        for type_ in types_to_remove {
            type_.remove(&mut self.schema)?;
        }
        error.into_result()
    }

    fn add_shareable(&mut self) -> Result<(), FederationError> {
        let schema = &mut self.schema;
        let Some(metadata) = &schema.subgraph_metadata else {
            return Ok(());
        };

        // Get key directive and shareable directive from the metadata
        let key_directive = metadata
            .federation_spec_definition()
            .key_directive_definition(schema)?;

        let shareable_directive = metadata
            .federation_spec_definition()
            .shareable_directive_definition(schema)?;

        let mut fields_to_add_shareable = vec![];
        let mut types_to_add_shareable = vec![];
        for type_pos in schema.get_types() {
            let has_key_directive = type_pos.has_applied_directive(schema, &key_directive.name);
            let is_root_type = Self::is_root_type(schema, &type_pos);
            let TypeDefinitionPosition::Object(obj_pos) = type_pos else {
                continue;
            };
            let obj_name = &obj_pos.type_name;
            // Skip Subscription root type - no shareable needed
            if obj_pos.type_name.as_str() == SchemaRootDefinitionKind::Subscription.to_string() {
                continue;
            }
            if has_key_directive || is_root_type {
                for field in obj_pos.fields(schema.schema())? {
                    let obj_field = FieldDefinitionPosition::Object(field.clone());
                    if metadata
                        .is_field_shareable(&obj_field)
                    {
                        continue;
                    }
                    let Some(entries) = self.object_type_map.get(obj_name) else {
                        continue;
                    };

                    let type_in_other_subgraphs = entries.iter().any(|(subgraph_name, info)| {
                        if subgraph_name != self.expanded_info.subgraph_name.as_str()
                            && (info.metadata.is_field_external(&obj_field)
                                || info.metadata.is_field_partially_external(&obj_field))
                        {
                            return true;
                        }
                        false
                    });
                    if type_in_other_subgraphs
                        && !obj_field.has_applied_directive(schema, &shareable_directive.name)
                    {
                        fields_to_add_shareable.push(field.clone());
                    }
                }
            } else {
                let Some(entries) = self.object_type_map.get(obj_name) else {
                    continue;
                };
                let type_in_other_subgraphs = entries.iter().any(|(subgraph_name, _info)| {
                    if subgraph_name != self.expanded_info.subgraph_name.as_str() {
                        return true;
                    }
                    false
                });
                if type_in_other_subgraphs
                    && !obj_pos.has_applied_directive(schema, &shareable_directive.name)
                {
                    types_to_add_shareable.push(obj_pos.clone());
                }
            }
        }
        let shareable_directive_name = shareable_directive.name.clone();
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

    fn remove_tag_on_external(&mut self) -> Result<(), FederationError> {
        let schema = &mut self.schema;
        let applications = schema.tag_directive_applications()?;
        let mut to_delete: Vec<(FieldDefinitionPosition, Node<Directive>)> = vec![];
        if let Some(metadata) = &schema.subgraph_metadata {
            applications
                .iter()
                .try_for_each(|application| -> Result<(), FederationError> {
                    if let Ok(application) = (*application).as_ref() {
                        if let Ok(target) = FieldDefinitionPosition::try_from(application.target.clone()) {
                            if metadata
                                .external_metadata()
                                .is_external(&target)
                            {
                                let used_in_other_definitions =
                                    self.subgraphs.iter().fallible_any(
                                        |(name, subgraph)| -> Result<bool, FederationError> {
                                            if &self.expanded_info.subgraph_name != name {
                                                // check to see if the field is external in the other subgraphs
                                                if let Some(other_metadata) =
                                                    &subgraph.schema().subgraph_metadata
                                                {
                                                    if !other_metadata
                                                        .external_metadata()
                                                        .is_external(&target)
                                                    {
                                                        // at this point, we need to check to see if there is a @tag directive on the other subgraph that matches the current application
                                                        let other_applications = subgraph
                                                            .schema()
                                                            .tag_directive_applications()?;
                                                        return other_applications.iter().fallible_any(
                                                            |other_app_result| -> Result<bool, FederationError> {
                                                                if let Ok(other_tag_directive) =
                                                                    (*other_app_result).as_ref()
                                                                {
                                                                    if application.target
                                                                        == other_tag_directive.target
                                                                        && application.arguments.name
                                                                            == other_tag_directive
                                                                                .arguments
                                                                                .name
                                                                    {
                                                                        return Ok(true);
                                                                    }
                                                                }
                                                                Ok(false)
                                                            },
                                                        );
                                                    }
                                                }
                                            }
                                            Ok(false)
                                        },
                                    );
                                if used_in_other_definitions? {
                                    // remove @tag
                                    to_delete.push((
                                        target.clone(),
                                        application.directive.clone(),
                                    ));
                                }
                            }
                        }
                    }
                    Ok(())
                })?;
        }
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

#[cfg(test)]
mod tests {
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

        let [s1, _s2]: [Subgraph<Expanded>; 2] = upgrade_subgraphs_if_necessary(vec![s1, s2]).expect("upgrades schema").try_into().expect("Expected 2 elements");

        insta::assert_snapshot!(
            s1.schema().schema().to_string(), @r###"
            schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.4", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject"]) {
              query: Query
            }

            directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

            directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

            directive @requires(fields: federation__FieldSet!) on FIELD_DEFINITION

            directive @provides(fields: federation__FieldSet!) on FIELD_DEFINITION

            directive @external(reason: String) on OBJECT | FIELD_DEFINITION

            directive @shareable repeatable on OBJECT | FIELD_DEFINITION

            directive @override(from: String!) on FIELD_DEFINITION

            directive @tag repeatable on ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

            directive @composeDirective(name: String!) repeatable on SCHEMA

            type Query {
              products: [Product!]! @provides(fields: "description")
              _entities(representations: [_Any!]!): [_Entity]!
              _service: _Service!
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

            scalar federation__FieldSet

            scalar _Any

            type _Service @shareable {
              sdl: String
            }

            union _Entity = Product
        "###
        );
    }

    #[test]
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

        let [s]: [Subgraph<Expanded>; 1] = upgrade_subgraphs_if_necessary(vec![s]).expect("upgrades schema").try_into().expect("Expected 1 element");

        insta::assert_snapshot!(
            s.schema().schema().to_string(),
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

        let [s1, s2]: [Subgraph<Expanded>; 2] = upgrade_subgraphs_if_necessary(vec![s1, s2]).expect("upgrades schema").try_into().expect("Expected 2 elements");

        let type_a_in_s1 = s1.schema().schema().get_object("A").unwrap();
        let type_a_in_s2 = s2.schema().schema().get_object("A").unwrap();

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
        // Note that this test both validates the rejection of fed1 subgraph when @interfaceObject is used somewhere, but also
        // illustrate why we do so: fed1 schema can use @key on interface for backward compatibility, but it is ignored and
        // the schema upgrader removes them. Given that actual support for @key on interfaces is necesarry to make @interfaceObject
        // work, it would be really confusing to not reject the example below right away, since it "looks" like it the @key on
        // the interface in the 2nd subgraph should work, but it actually won't.

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

        assert_eq!(
            errors.to_string(),
            r#"The @interfaceObject directive can only be used if all subgraphs have federation 2 subgraph schema (schema with a `@link` to "https://specs.apollo.dev/federation" version 2.0 or newer): @interfaceObject is used in subgraph "s1" but subgraph "s2" is not a federation 2 subgraph schema."#
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

        let [s1, s2]: [Subgraph<Expanded>; 2] = upgrade_subgraphs_if_necessary(vec![s1, s2]).expect("upgrades schema").try_into().expect("Expected 2 elements");

        // 2 things must happen here:
        // 1. the @external on type `T` in s2 should be removed, as @external on types were no-ops in fed1 (but not in fed2 anymore, hence the removal)
        // 2. field `T.x` in s1 must be marked @shareable since it is resolved by s2 (since again, it's @external annotation is ignored).

        assert!(
            s1.schema()
                .schema()
                .types
                .get("T")
                .is_some_and(|t| !t.directives().has("external"))
        );
        assert!(
            s2.schema()
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

        let [subgraph]: [Subgraph<Expanded>; 1] = upgrade_subgraphs_if_necessary(vec![subgraph]).expect("upgrades schema").try_into().expect("Expected 1 element");
        // Note: this test mostly exists for dev awareness. By design, this will
        // always require updating when the fed spec version is updated, so hopefully
        // you're reading this comment. Existing schemas which don't include a @link
        // directive usage will be upgraded to the latest version of the federation
        // spec. The downstream effect of this auto-upgrading behavior is:
        //
        // GraphOS users who select the new build track you're going to introduce will
        // immediately start composing with the latest specs without having to update
        // their @link federation spec version in any of their subgraphs. For this to
        // be ok, they need to first update to a router version which supports
        // whatever changes you've introduced in the new spec version. Take care to
        // ensure that things are released in the correct order.
        //
        // Ideally, in the future we ensure that GraphOS users are on a version of
        // router that supports the build pipeline they're upgrading to, but that
        // mechanism isn't in place yet.
        // - Trevor
        insta::assert_snapshot!(
            subgraph.schema().schema().to_string(),
            r#"
            schema
                @link(url: "https://specs.apollo.dev/link/v1.0")
                @link(url: "https://specs.apollo.dev/federation/v2.4", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject"])
            {
                query: Query
            }
        "#
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

        let [subgraph1, subgraph2]: [Subgraph<Expanded>; 2] = upgrade_subgraphs_if_necessary(vec![subgraph1, subgraph2]).expect("upgrades schema").try_into().expect("Expected 2 elements");

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
}
