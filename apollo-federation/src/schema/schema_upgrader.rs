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
use crate::schema::SubgraphMetadata;
use crate::subgraph::typestate::Expanded;
use crate::subgraph::typestate::Subgraph;
use crate::utils::FallibleIterator;

#[derive(Clone, Debug)]
struct SchemaUpgrader<'a> {
    schema: FederationSchema,
    original_subgraph: &'a Subgraph<Expanded>,
    subgraphs: &'a [Subgraph<Expanded>],
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
pub(crate) fn upgrade_subgraphs_if_necessary(
    subgraphs: &mut [Subgraph<Expanded>],
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
    fn has_extension_elements(
        &self,
        type_pos: &TypeDefinitionPosition,
    ) -> Result<bool, FederationError>;
    fn has_non_extension_elements(
        &self,
        type_pos: &TypeDefinitionPosition,
    ) -> Result<bool, FederationError>;
    fn is_root_type(&self, type_pos: &TypeDefinitionPosition) -> Result<bool, FederationError>;
}

// Implementation of the extension trait for FederationSchema
impl FederationSchemaExt for FederationSchema {
    fn has_extension_elements(
        &self,
        type_pos: &TypeDefinitionPosition,
    ) -> Result<bool, FederationError> {
        // Check if a type has any extension elements
        // This would check if the type has any extensions defined in the schema
        match type_pos {
            TypeDefinitionPosition::Object(obj_pos) => {
                let schema = self.schema();
                let type_name = &obj_pos.type_name;
                if let Some(ExtendedType::Object(obj)) = schema.types.get(type_name) {
                    return Ok(!obj.extensions().is_empty());
                }
                Ok(false)
            }
            TypeDefinitionPosition::Interface(itf_pos) => {
                let schema = self.schema();
                let type_name = &itf_pos.type_name; // Fixed: access field directly
                if let Some(ExtendedType::Interface(itf)) = schema.types.get(type_name) {
                    // Fixed: call extensions() method, not access as field
                    return Ok(!itf.extensions().is_empty());
                }
                Ok(false)
            }
            _ => Ok(false),
        }
    }

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

    fn is_root_type(&self, type_pos: &TypeDefinitionPosition) -> Result<bool, FederationError> {
        // Check if a type is a root type (Query, Mutation, or Subscription)
        // Get the type name based on the type position
        let type_name = match type_pos {
            TypeDefinitionPosition::Object(pos) => &pos.type_name,
            TypeDefinitionPosition::Interface(pos) => &pos.type_name,
            _ => return Ok(false), // Not a root type if not object or interface
        };

        // Check for root types by querying the schema directly
        // This is a simplified implementation for compilation purposes

        // Check for known root type names
        if type_name.as_str() == "Query"
            || type_name.as_str() == "Mutation"
            || type_name.as_str() == "Subscription"
        {
            return Ok(true);
        }

        Ok(false)
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
        subgraphs: &'a [Subgraph<Expanded>],
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
            if self.is_root_type_extension(&type_pos)?
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

    // it certainly seems like this and is_root_type_extension could be combined, but I'm copying the functionality as-is for now
    fn is_federation_type_extension(
        &self,
        type_pos: &TypeDefinitionPosition,
    ) -> Result<bool, FederationError> {
        if !matches!(
            type_pos,
            TypeDefinitionPosition::Object(_) | TypeDefinitionPosition::Interface(_)
        ) {
            return Ok(false);
        }

        let schema = &self.schema;

        // because this is for Fed1, the directive may not be renamed
        let fed1_extends_directive_name = "extends";
        let has_extends_directive = type_pos
            .has_applied_directive(schema, &Name::new_unchecked(fed1_extends_directive_name));

        // Use the FederationSchemaExt trait
        let has_extension_elements = schema.has_extension_elements(type_pos)?;
        let has_non_extension_elements = schema.has_non_extension_elements(type_pos)?;

        Ok(has_extends_directive || has_extension_elements || !has_non_extension_elements)
    }

    fn is_root_type_extension(
        &self,
        type_pos: &TypeDefinitionPosition,
    ) -> Result<bool, FederationError> {
        // Check if this is a root type (Query, Mutation, Subscription) that is declared as an extension

        let TypeDefinitionPosition::Object(_) = type_pos else {
            return Ok(false);
        };
        let schema = &self.schema;

        let is_root_type = schema.is_root_type(type_pos)?;
        if !is_root_type {
            return Ok(false);
        }

        // because this is for Fed1, the directive may not be renamed
        let fed1_extends_directive_name = "extends";
        let has_extends_directive = type_pos
            .has_applied_directive(schema, &Name::new_unchecked(fed1_extends_directive_name));

        // Use the FederationSchemaExt trait
        let has_extension_elements = schema.has_extension_elements(type_pos)?;
        let has_non_extension_elements = schema.has_non_extension_elements(type_pos)?;

        Ok(has_extends_directive || (has_extension_elements && !has_non_extension_elements))
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

    fn fix_inactive_provides_and_requires(&self) {
        todo!();
    }

    fn remove_type_extensions(&self) {
        todo!();
    }

    fn remove_directives_on_interface(&mut self) -> Result<(), FederationError> {
        let schema = &mut self.schema;
        let Some(metadata) = &schema.subgraph_metadata else {
            return Ok(());
        };

        let _provides_directive = metadata
            .federation_spec_definition()
            .provides_directive_definition(schema)?;

        let _requires_directive = metadata
            .federation_spec_definition()
            .requires_directive_definition(schema)?;

        let _key_directive = metadata
            .federation_spec_definition()
            .key_directive_definition(schema)?;

        todo!();
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

    fn remove_unused_externals(&self) {
        todo!();
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
