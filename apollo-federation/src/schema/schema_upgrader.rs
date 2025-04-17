use std::collections::HashSet;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Value;
use apollo_compiler::collections::HashMap;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;

use super::FederationSchema;
use super::TypeDefinitionPosition;
use super::position::FieldDefinitionPosition;
use super::position::InterfaceFieldDefinitionPosition;
use super::position::InterfaceTypeDefinitionPosition;
use super::position::ObjectFieldDefinitionPosition;
use super::position::ObjectTypeDefinitionPosition;
use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::schema::SubgraphMetadata;
use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceTypeDefinitionPosition;
use crate::subgraph::typestate::Expanded;
use crate::subgraph::typestate::Subgraph;
use crate::supergraph::remove_inactive_requires_and_provides_from_subgraph;
use crate::utils::FallibleIterator;

#[derive(Clone, Debug)]
struct SchemaUpgrader<'a> {
    schema: FederationSchema,
    original_subgraph: &'a Subgraph<Expanded>,
    subgraphs: &'a [&'a mut Subgraph<Expanded>],
    #[allow(unused)]
    object_type_map: &'a HashMap<Name, HashMap<String, TypeInfo>>,
}

#[derive(Clone, Debug)]
#[allow(unused)]
struct TypeInfo {
    pos: TypeDefinitionPosition,
    metadata: SubgraphMetadata,
}

#[allow(unused)]
// PORT_NOTE: In JS, this returns upgraded subgraphs along with a set of messages about what changed.
// However, those messages were never used, so we have omitted them here.
pub(crate) fn upgrade_subgraphs_if_necessary(
    subgraphs: &[&mut Subgraph<Expanded>],
) -> Result<(), FederationError> {
    // if all subgraphs are fed 2, there is no upgrade to be done
    if subgraphs
        .iter()
        .all(|subgraph| subgraph.metadata().is_fed_2_schema())
    {
        return Ok(());
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
    for subgraph in subgraphs.iter() {
        if !subgraph.schema().is_fed_2() {
            let mut upgrader = SchemaUpgrader::new(subgraph, subgraphs, &object_type_map)?;
            upgrader.upgrade()?;
        }
    }
    // TODO: Return federation_subgraphs
    todo!();
}

// Extensions for FederationSchema to provide additional functionality
trait FederationSchemaExt {
    fn has_non_extension_elements(
        &self,
        type_pos: &TypeDefinitionPosition,
    ) -> Result<bool, FederationError>;
}

// Implementation of the extension trait for FederationSchema
impl FederationSchemaExt for FederationSchema {
    // a type has a non-extension element if the following cases
    // 1. There is a directive on the non-extended type
    // 2. There is a field on the non-extended type
    // 3. There is an interface implementation on the non-extended type
    fn has_non_extension_elements(
        &self,
        type_pos: &TypeDefinitionPosition,
    ) -> Result<bool, FederationError> {
        let schema = self.schema();
        let result = match type_pos {
            TypeDefinitionPosition::Object(obj_pos) => {
                let node_obj = obj_pos.get(schema)?;
                let directive_on_non_extended_type = node_obj
                    .directives
                    .iter()
                    .any(|directive| directive.origin.extension_id().is_some());
                let field_on_non_extended_type = obj_pos.fields(schema)?.fallible_any(
                    |field| -> Result<bool, FederationError> {
                        Ok(field.get(schema)?.origin.extension_id().is_some())
                    },
                )?;
                let interface_implemented_on_non_extended_type = node_obj
                    .implements_interfaces
                    .iter()
                    .any(|itf| itf.origin.extension_id().is_some());
                directive_on_non_extended_type
                    || field_on_non_extended_type
                    || interface_implemented_on_non_extended_type
            }
            TypeDefinitionPosition::Interface(itf_pos) => {
                let node_itf = itf_pos.get(schema)?;
                let directive_on_non_extended_type = node_itf
                    .directives
                    .iter()
                    .any(|directive| directive.origin.extension_id().is_some());
                let field_on_non_extended_type = itf_pos.fields(schema)?.fallible_any(
                    |field| -> Result<bool, FederationError> {
                        Ok(field.get(schema)?.origin.extension_id().is_some())
                    },
                )?;
                let interface_implemented_on_non_extended_type = node_itf
                    .implements_interfaces
                    .iter()
                    .any(|itf| itf.origin.extension_id().is_some());
                directive_on_non_extended_type
                    || field_on_non_extended_type
                    || interface_implemented_on_non_extended_type
            }
            _ => false,
        };
        Ok(result)
    }
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
        subgraphs: &'a [&'a mut Subgraph<Expanded>],
        object_type_map: &'a HashMap<Name, HashMap<String, TypeInfo>>,
    ) -> Result<Self, FederationError> {
        Ok(SchemaUpgrader {
            schema: original_subgraph.schema().clone(), // TODO: Don't think we should be cloning here
            original_subgraph,
            subgraphs,
            object_type_map,
        })
    }

    #[allow(unused)]
    fn upgrade(&mut self) -> Result<Subgraph<Expanded>, FederationError> {
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

        todo!();
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
                            subgraph_name.as_str() != self.original_subgraph.name.as_str()
                        })
                        .fallible_any(|(_, type_info)| {
                            schema.has_non_extension_elements(&type_info.pos)
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
                .join(",");

            return Ok(Some(combined));
        }
        if let Some(enum_value) = arg.as_enum() {
            return Ok(Some(enum_value.as_str().to_string()));
        }
        Ok(None)
    }

    fn fix_federation_directives_arguments(&mut self) -> Result<(), FederationError> {
        let schema = &mut self.schema;

        // both @provides and @requirers will only have an object_fields referencer
        for directive_name in ["requires", "provides"] {
            let mut change_list: Vec<(Node<Directive>, ObjectFieldDefinitionPosition, String)> =
                Vec::new();

            let referencers = schema.referencers().get_directive(directive_name)?;

            for field in &referencers.object_fields {
                if let Ok(field_type) = field.get(schema.schema()) {
                    let directives = &field_type.directives;
                    for directive in directives.get_all(directive_name) {
                        if let Ok(arg) = directive.argument_by_name("fields", schema.schema()) {
                            if let Some(new_fields_string) =
                                SchemaUpgrader::<'a>::make_fields_string_if_not(arg)?
                            {
                                change_list.push((
                                    directive.clone(),
                                    field.clone(),
                                    new_fields_string.clone(),
                                ));
                            }
                        }
                    }
                }
            }

            for (directive, field, new_fields_string) in change_list {
                // First add the new directive with the combined fields string
                let arg = Node::new(Argument {
                    name: Name::new_unchecked("fields"),
                    value: Node::new(Value::String(new_fields_string)),
                });
                field.remove_directive(schema, &directive);
                field.insert_directive(
                    schema,
                    Node::new(Directive {
                        name: directive.name.clone(),
                        arguments: vec![arg],
                    }),
                )?;
            }
        }

        // now do the exact same thing for @key. The difference is that the directive location will be object_types
        // rather than object_fields
        let mut change_list: Vec<(Component<Directive>, ObjectTypeDefinitionPosition, String)> =
            Vec::new();
        let referencers = schema.referencers().get_directive("key")?;

        for field in &referencers.object_types {
            if let Ok(field_type) = field.get(schema.schema()) {
                let directives = &field_type.directives;
                for directive in directives.get_all("key") {
                    if let Ok(arg) = directive.argument_by_name("fields", schema.schema()) {
                        if let Some(new_fields_string) =
                            SchemaUpgrader::<'a>::make_fields_string_if_not(arg)?
                        {
                            change_list.push((
                                directive.clone(),
                                field.clone(),
                                new_fields_string.clone(),
                            ));
                        }
                    }
                }
            }
        }
        for (directive, field, new_fields_string) in change_list {
            // First add the new directive with the combined fields string
            let arg = Node::new(Argument {
                name: Name::new_unchecked("fields"),
                value: Node::new(Value::String(new_fields_string)),
            });
            field.remove_directive(schema, &directive);
            field.insert_directive(
                schema,
                Component::new(Directive {
                    name: directive.name.clone(),
                    arguments: vec![arg],
                }),
            )?;
        }
        Ok(())
    }

    // Skip the trait definitions for now - we'll use a simpler approach

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
        let mut to_delete: Vec<(ObjectFieldDefinitionPosition, Node<Directive>)> = vec![];
        for (obj_name, ty) in schema.schema().types.iter() {
            let ExtendedType::Object(obj) = ty else {
                continue;
            };
            let object_pos = ObjectTypeDefinitionPosition::new(obj_name.clone());
            for (field_name, field) in &obj.fields {
                let pos = object_pos.field(field_name.clone());
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

    fn remove_external_on_type_extensions(&mut self) -> Result<(), FederationError> {
        todo!();
    }

    fn fix_inactive_provides_and_requires(&mut self) -> Result<(), FederationError> {
        remove_inactive_requires_and_provides_from_subgraph(
            self.original_subgraph.schema(),
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
            .original_subgraph
            .extends_directive_name()?
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
        if !matches!(pos, TypeDefinitionPosition::Object(_)) || !self.is_root_type(pos) {
            return false;
        }
        let Ok(ty) = pos.get(self.schema.schema()) else {
            return false;
        };
        let has_extends_directive = self
            .original_subgraph
            .extends_directive_name()
            .ok()
            .flatten()
            .is_some_and(|extends| ty.directives().has(extends.as_str()));

        has_extends_directive
            || (Self::has_extension_elements(ty) && !Self::has_non_extension_elements(ty))
    }

    fn is_root_type(&self, ty: &TypeDefinitionPosition) -> bool {
        self.schema
            .schema()
            .schema_definition
            .iter_root_operations()
            .any(|op| op.1.as_str() == ty.type_name().as_str())
    }

    fn remove_directives_on_interface(&mut self) -> Result<(), FederationError> {
        if let Some(key) = self.original_subgraph.key_directive_name()? {
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
                    if let Some(provides) = self.original_subgraph.provides_directive_name()? {
                        field.remove_directive_name(&mut self.schema, &provides);
                    }
                    if let Some(requires) = self.original_subgraph.requires_directive_name()? {
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
                    if self
                        .original_subgraph
                        .metadata()
                        .is_field_external(&field_def)
                        && !self.original_subgraph.metadata().is_field_used(&field_def)
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
        todo!();
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
                                        |subgraph| -> Result<bool, FederationError> {
                                            if self.original_subgraph.name != subgraph.name {
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

    const FEDERATION2_LINK_WITH_AUTO_EXPANDED_IMPORTS_UPGRADED: &str = r#"@link(url: "https://specs.apollo.dev/federation/v2.4", import: ["@key", "@requires", "@provides", "@external", "@tag", "@extends", "@shareable", "@inaccessible", "@override", "@composeDirective", "@interfaceObject"])"#;

    #[ignore = "not yet implemented"]
    #[test]
    fn upgrades_complex_schema() {
        let mut s1 = Subgraph::parse(
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
                x: Int @provides(fields: "x")
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
        let mut s2 = Subgraph::parse(
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

        upgrade_subgraphs_if_necessary(&[&mut s1, &mut s2]).expect("upgrades schema");

        insta::assert_snapshot!(
            s1.schema().schema().to_string(),
            r#"
            schema
                FEDERATION2_LINK_WITH_AUTO_EXPANDED_IMPORTS_UPGRADED
            {
                query: Query
            }

            type Query {
                products: [Product!]! @provides(fields: "description")
            }

            interface I {
                upc: ID!
                description: String
            }

            type Product implements I
                @key(fields: "upc")
            {
                upc: ID!
                inventory: Int
                description: String @external
            }

            type Random {
                x: Int
            }

            extend type Random {
                y: Int
            }
        "#
            .replace(
                "FEDERATION2_LINK_WITH_AUTO_EXPANDED_IMPORTS_UPGRADED",
                FEDERATION2_LINK_WITH_AUTO_EXPANDED_IMPORTS_UPGRADED
            )
        );
    }

    #[ignore = "not yet implemented"]
    #[test]
    fn update_federation_directive_non_string_arguments() {
        let mut s = Subgraph::parse(
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

        upgrade_subgraphs_if_necessary(&[&mut s]).expect("upgrades schema");

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

    #[ignore = "not yet implemented"]
    #[test]
    fn remove_tag_on_external_field_if_found_on_definition() {
        let mut s1 = Subgraph::parse(
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

        let mut s2 = Subgraph::parse(
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

        upgrade_subgraphs_if_necessary(&[&mut s1, &mut s2]).expect("upgrades schema");

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

    #[ignore = "not yet implemented"]
    #[test]
    fn reject_interface_object_usage_if_not_all_subgraphs_are_fed2() {
        // Note that this test both validates the rejection of fed1 subgraph when @interfaceObject is used somewhere, but also
        // illustrate why we do so: fed1 schema can use @key on interface for backward compatibility, but it is ignored and
        // the schema upgrader removes them. Given that actual support for @key on interfaces is necesarry to make @interfaceObject
        // work, it would be really confusing to not reject the example below right away, since it "looks" like it the @key on
        // the interface in the 2nd subgraph should work, but it actually won't.

        let mut s1 = Subgraph::parse("s1", "", r#"
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

        let mut s2 = Subgraph::parse(
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

        let errors = upgrade_subgraphs_if_necessary(&[&mut s1, &mut s2]).expect_err("should fail");

        assert_eq!(
            errors.to_string(),
            r#"The @interfaceObject directive can only be used if all subgraphs have federation 2 subgraph schema (schema with a `@link` to "https://specs.apollo.dev/federation" version 2.0 or newer): @interfaceObject is used in subgraph "s1" but subgraph "s2" is not a federation 2 subgraph schema."#
        );
    }

    #[ignore = "not yet implemented"]
    #[test]
    fn handles_addition_of_shareable_when_external_is_used_on_type() {
        let mut s1 = Subgraph::parse(
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

        let mut s2 = Subgraph::parse(
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

        upgrade_subgraphs_if_necessary(&[&mut s1, &mut s2]).expect("upgrades schema");

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

    #[ignore = "not yet implemented"]
    #[test]
    fn fully_upgrades_schema_with_no_link_directives() {
        let mut subgraph = Subgraph::parse(
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

        upgrade_subgraphs_if_necessary(&[&mut subgraph]).expect("upgrades schema");
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

    #[ignore = "not yet implemented"]
    #[test]
    fn does_not_add_shareable_to_subscriptions() {
        let mut subgraph1 = Subgraph::parse(
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

        let mut subgraph2 = Subgraph::parse(
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

        upgrade_subgraphs_if_necessary(&[&mut subgraph1, &mut subgraph2]).expect("upgrades schema");

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
