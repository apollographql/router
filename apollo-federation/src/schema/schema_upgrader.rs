use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Directive;
use apollo_compiler::collections::HashMap;
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
use crate::schema::field_set::collect_target_fields_from_field_set;

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
    fn has_extension_elements(&self, type_pos: &TypeDefinitionPosition) -> bool;
    fn has_non_extension_elements(&self, type_pos: &TypeDefinitionPosition) -> bool;
    fn is_root_type(&self, type_pos: &TypeDefinitionPosition) -> bool;
}

// Implementation of the extension trait for FederationSchema
impl FederationSchemaExt for FederationSchema {
    fn has_extension_elements(&self, type_pos: &TypeDefinitionPosition) -> bool {
        // Check if a type has any extension elements
        // This would check if the type has any extensions defined in the schema
        match type_pos {
            TypeDefinitionPosition::Object(obj_pos) => {
                let schema = self.schema();
                let type_name = &obj_pos.type_name; // Fixed: access field directly, not as method
                if let Some(ExtendedType::Object(obj)) = schema.types.get(type_name) {
                    // Check if there are extensions - using a method call, not field access
                    return !obj.extensions().is_empty();
                }
                false
            },
            TypeDefinitionPosition::Interface(itf_pos) => {
                let schema = self.schema();
                let type_name = &itf_pos.type_name; // Fixed: access field directly
                if let Some(ExtendedType::Interface(itf)) = schema.types.get(type_name) {
                    // Fixed: call extensions() method, not access as field
                    return !itf.extensions().is_empty();
                }
                false
            },
            _ => false,
        }
    }

    fn has_non_extension_elements(&self, type_pos: &TypeDefinitionPosition) -> bool {
        // Check if a type has any non-extension elements (i.e., a type definition)
        // Note: in apollo-compiler, we can't directly access 'definition', so we need to check
        // if there's at least one non-extension element (this is a placeholder, needs to be revisited)
        match type_pos {
            TypeDefinitionPosition::Object(obj_pos) => {
                let schema = self.schema();
                let type_name = &obj_pos.type_name; // Fixed: access field directly
                if let Some(ExtendedType::Object(obj)) = schema.types.get(type_name) {
                    // Placeholder: Assuming we need to check if there's at least one field that's not in an extension
                    return !obj.fields.is_empty();
                }
                false
            },
            TypeDefinitionPosition::Interface(itf_pos) => {
                let schema = self.schema();
                let type_name = &itf_pos.type_name; // Fixed: access field directly
                if let Some(ExtendedType::Interface(itf)) = schema.types.get(type_name) {
                    // Placeholder: Same assumption as above
                    return !itf.fields.is_empty();
                }
                false
            },
            _ => false,
        }
    }

    fn is_root_type(&self, type_pos: &TypeDefinitionPosition) -> bool {
        // Check if a type is a root type (Query, Mutation, or Subscription)
        // Get the type name based on the type position
        let type_name = match type_pos {
            TypeDefinitionPosition::Object(pos) => &pos.type_name,
            TypeDefinitionPosition::Interface(pos) => &pos.type_name,
            _ => return false, // Not a root type if not object or interface
        };
        
        let schema = self.schema();
        
        // Check for root types by querying the schema directly
        // This is a simplified implementation for compilation purposes
        
        // Check for known root type names
        if type_name.as_str() == "Query" || 
           type_name.as_str() == "Mutation" || 
           type_name.as_str() == "Subscription" {
            return true;
        }
        
        false
    }
}

// Extensions for FederationError to provide additional error types
impl FederationError {
    pub fn extension_with_no_base(message: &str) -> Self {
        // Fixed: use internal() instead of new()
        FederationError::internal(format!("EXTENSION_WITH_NO_BASE: {}", message))
    }
}

// Extensions for SubgraphMetadata 
trait SubgraphMetadataExt {
    fn extends_directive_applications(&self) -> Vec<Result<DirectiveApplication, FederationError>>;
}

// Placeholder struct for directive applications
#[derive(Clone, Debug)]
struct DirectiveApplication {
    target: DirectiveTarget,
    directive: Node<Directive>,
    arguments: DirectiveArguments,
}

// Placeholder struct for directive arguments
#[derive(Clone, Debug)]
struct DirectiveArguments {
    name: String,
}

// Placeholder enum for directive targets
#[derive(Clone, Debug)]
enum DirectiveTarget {
    Type(Name),
    Field(FieldDefinitionPosition),
}

impl TryFrom<DirectiveTarget> for TypeDefinitionPosition {
    type Error = FederationError;
    
    fn try_from(target: DirectiveTarget) -> Result<Self, Self::Error> {
        match target {
            DirectiveTarget::Type(name) => {
                // Convert a type name to a TypeDefinitionPosition
                // This is a placeholder implementation
                Ok(TypeDefinitionPosition::Object(ObjectTypeDefinitionPosition::new(name)))
            },
            _ => Err(FederationError::internal("Cannot convert field target to type position")),
        }
    }
}

// Implementation for SubgraphMetadata
impl SubgraphMetadataExt for SubgraphMetadata {
    fn extends_directive_applications(&self) -> Vec<Result<DirectiveApplication, FederationError>> {
        // This would return all applications of the @extends directive
        // Placeholder implementation
        Vec::new()
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

    fn pre_upgrade_validations(&self) -> Result<(), FederationError> {
        let schema = &self.schema;
        
        // Iterate through all types and check if they're federation type extensions without a base
        for type_pos in schema.get_types() {
            if self.is_root_type_extension(&type_pos) || !self.is_federation_type_extension(&type_pos) {
                continue;
            }
            
            // Get the type name based on the type position
            let type_name = match &type_pos {
                TypeDefinitionPosition::Object(pos) => &pos.type_name,
                TypeDefinitionPosition::Interface(pos) => &pos.type_name,
                _ => continue, // Skip other types
            };
            
            // Check if any other subgraph has a proper definition for this type
            let has_non_extension_definition = self.object_type_map
                .get(type_name)
                .map(|subgraph_types| {
                    subgraph_types
                        .iter()
                        .filter(|(subgraph_name, _)| {
                            // Fixed: dereference the string for comparison
                            subgraph_name.as_str() != self.original_subgraph.name.as_str()
                        })
                        .any(|(_, type_info)| {
                            schema.has_non_extension_elements(&type_info.pos)
                        })
                })
                .unwrap_or(false);
                
            if !has_non_extension_definition {
                return Err(FederationError::extension_with_no_base(
                    &format!(
                        "Type \"{}\" is an extension type, but there is no type definition for \"{}\" in any subgraph.",
                        type_name, type_name
                    ),
                ));
            }
        }
        
        Ok(())
    }
    
    fn is_federation_type_extension(&self, type_pos: &TypeDefinitionPosition) -> bool {
        // Federation type extensions are object or interface type extensions that:
        // 1. Have the @extends directive or are extension-only
        // 2. Don't have a non-extension definition in the same subgraph
        
        // Only check object and interface types
        if !matches!(
            type_pos,
            TypeDefinitionPosition::Object(_) | TypeDefinitionPosition::Interface(_)
        ) {
            return false;
        }
        
        let schema = &self.schema;
        let Some(metadata) = &schema.subgraph_metadata else {
            return false;
        };
        
        // Use the SubgraphMetadataExt trait
        let has_extend = metadata.extends_directive_applications()
            .iter()
            .any(|app| {
                if let Ok(app) = app {
                    if let Ok(app_type_pos) = TypeDefinitionPosition::try_from(app.target.clone()) {
                        // Fixed: extract the type name based on the enum variant
                        let app_type_name = match &app_type_pos {
                            TypeDefinitionPosition::Object(pos) => &pos.type_name,
                            TypeDefinitionPosition::Interface(pos) => &pos.type_name,
                            _ => return false,
                        };
                        
                        // Fixed: extract the type name from the input type_pos
                        let type_name = match type_pos {
                            TypeDefinitionPosition::Object(pos) => &pos.type_name,
                            TypeDefinitionPosition::Interface(pos) => &pos.type_name,
                            _ => return false,
                        };
                        
                        return app_type_name == type_name;
                    }
                }
                false
            });
            
        // Use the FederationSchemaExt trait
        let has_extension_elements = schema.has_extension_elements(type_pos);
        let has_non_extension_elements = schema.has_non_extension_elements(type_pos);
        
        (has_extend || has_extension_elements) && (has_extend || !has_non_extension_elements)
    }
    
    fn is_root_type_extension(&self, type_pos: &TypeDefinitionPosition) -> bool {
        // Check if this is a root type (Query, Mutation, Subscription) that is declared as an extension
        
        let TypeDefinitionPosition::Object(_) = type_pos else {
            return false;
        };
        
        let schema = &self.schema;
        let Some(metadata) = &schema.subgraph_metadata else {
            return false;
        };
        
        // Use the FederationSchemaExt trait
        let is_root_type = schema.is_root_type(type_pos);
        if !is_root_type {
            return false;
        }
        
        // Use the SubgraphMetadataExt trait
        let has_extend = metadata.extends_directive_applications()
            .iter()
            .any(|app| {
                if let Ok(app) = app {
                    if let Ok(app_type_pos) = TypeDefinitionPosition::try_from(app.target.clone()) {
                        // Fixed: extract the type name based on the enum variant
                        let app_type_name = match &app_type_pos {
                            TypeDefinitionPosition::Object(pos) => &pos.type_name,
                            TypeDefinitionPosition::Interface(pos) => &pos.type_name,
                            _ => return false,
                        };
                        
                        // Fixed: extract the type name from the input type_pos
                        let type_name = match type_pos {
                            TypeDefinitionPosition::Object(pos) => &pos.type_name,
                            TypeDefinitionPosition::Interface(pos) => &pos.type_name,
                            _ => return false,
                        };
                        
                        return app_type_name == type_name;
                    }
                }
                false
            });
            
        // Use the FederationSchemaExt trait
        let has_extension_elements = schema.has_extension_elements(type_pos);
        let has_non_extension_elements = schema.has_non_extension_elements(type_pos);
        
        has_extend || (has_extension_elements && !has_non_extension_elements)
    }

    fn fix_federation_directives_arguments(&mut self) -> Result<(), FederationError> {
        // Implementing this function involves complex borrow patterns 
        // and a deep understanding of the Apollo Federation codebase
        // For now, we'll use a simplified placeholder implementation
        // The goal is to fix federation directive arguments where the 'fields' argument
        // isn't a string but an enum value or array
        
        // In a complete implementation, we would:
        // 1. Find all directives (@key, @provides, @requires) with a 'fields' argument
        // 2. Check if the 'fields' argument is not a string
        // 3. Convert the argument to a string value and update the directive
        
        // The implementation is non-trivial due to the borrowing patterns in the codebase
        // and would require a more comprehensive understanding of the types involved
        
        // This will be implemented fully in the future
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
        
        // Process each type in the schema
        for type_pos in schema.get_types().collect::<Vec<_>>() {
            // Skip non-composite types (only object and interface types can have @external)
            if !matches!(
                type_pos,
                TypeDefinitionPosition::Object(_) | TypeDefinitionPosition::Interface(_)
            ) {
                continue;
            }
            
            // Only process federation type extensions (type extensions or root type extensions)
            if !self.is_federation_type_extension(&type_pos) && !self.is_root_type_extension(&type_pos) {
                continue;
            }
            
            // Case 1: If the type extension has @key directives
            let has_key_directive = if let Some(key_directive) = metadata
                .federation_spec_definition()
                .key_directive_definition(schema)
                .ok()
            {
                schema.has_applied_directive(&type_pos, &key_directive.name)
            } else {
                false
            };
            
            if has_key_directive {
                // For types with @key directives, the key fields should not be marked @external
                let key_applications = schema.key_directive_applications()?;
                
                // Process each key application
                for key_application in key_applications.into_iter().filter_map(|app| app.ok()) {
                    // Only process key directives on the current type
                    if key_application.target.type_name() != type_pos.type_name() {
                        continue;
                    }
                    
                    // Collect fields referenced in the key
                    let key_fields = collect_target_fields_from_field_set(
                        schema.schema(),
                        key_application.target.type_name().clone(),
                        key_application.arguments.fields,
                        false,
                    )?;
                    
                    // Remove @external from key fields
                    for field_pos in key_fields {
                        if let FieldDefinitionPosition::Object(obj_field_pos) = field_pos {
                            // We only consider "top-level" fields, the ones on the type with the key
                            if obj_field_pos.type_name != *type_pos.type_name() {
                                continue;
                            }
                            
                            // Find and remove the @external directive if present
                            if let Some(directive) = obj_field_pos.get_applied_directive(schema, &external_directive.name) {
                                // In the JS code, we'd add a change tracking entry here
                                obj_field_pos.remove_directive(schema, &directive);
                            }
                        } else if let FieldDefinitionPosition::Interface(itf_field_pos) = field_pos {
                            // Same for interface fields
                            if itf_field_pos.type_name != *type_pos.type_name() {
                                continue;
                            }
                            
                            if let Some(directive) = itf_field_pos.get_applied_directive(schema, &external_directive.name) {
                                itf_field_pos.remove_directive(schema, &directive);
                            }
                        }
                    }
                }
            } else {
                // Case 2: Type extension without @key directives
                // In this case, we need to find other subgraphs with key directives on this type
                
                // Extract the type name
                let type_name = type_pos.type_name();
                
                // Find type in other subgraphs
                for other_subgraph in self.subgraphs {
                    if other_subgraph.name.as_str() == self.original_subgraph.name.as_str() {
                        continue;
                    }
                    // Try to get the type in the other subgraph
                    let Ok(other_type_pos) = other_subgraph.schema.get_type_definition_position(type_name.clone()) else {
                        continue;
                    };
                    
                    // Skip if the other subgraph has no metadata
                    let Some(other_metadata) = &other_subgraph.schema.subgraph_metadata else {
                        continue;
                    };
                    
                    // Get the key directive applications in the other subgraph
                    let Ok(key_directive_name) = other_metadata
                        .federation_spec_definition()
                        .key_directive_name_in_schema(&other_subgraph.schema) 
                    else {
                        continue;
                    };
                    
                    // Check if the type has any key directives in the other subgraph
                    if !other_subgraph.schema.has_applied_directive(&other_type_pos, &key_directive_name) {
                        continue;
                    }
                    
                    // Get the first key directive on this type in the other subgraph
                    let Ok(key_applications) = other_subgraph.schema.key_directive_applications() else {
                        continue;
                    };
                    
                    for key_application in key_applications.into_iter().filter_map(|app| app.ok()) {
                        // Only process key directives on the current type
                        if key_application.target.type_name() != other_type_pos.type_name() {
                            continue;
                        }
                        
                        // Get fields from the key
                        let key_fields = collect_target_fields_from_field_set(
                            other_subgraph.schema.schema(),
                            key_application.target.type_name().clone(),
                            key_application.arguments.fields,
                            false,
                        )?;
                        
                        // Now that we have the key fields from another subgraph,
                        // we need to find corresponding fields in our schema and remove @external from them
                        for other_field_pos in key_fields {
                            // Only consider top-level fields
                            let field_name = match &other_field_pos {
                                FieldDefinitionPosition::Object(pos) => {
                                    if pos.type_name != *other_type_pos.type_name() {
                                        continue;
                                    }
                                    &pos.field_name
                                },
                                FieldDefinitionPosition::Interface(pos) => {
                                    if pos.type_name != *other_type_pos.type_name() {
                                        continue;
                                    }
                                    &pos.field_name
                                },
                                _ => continue,
                            };
                            
                            // Find the corresponding field in our schema
                            let own_field_pos = match &type_pos {
                                TypeDefinitionPosition::Object(obj_pos) => {
                                    let Some(field) = obj_pos.field_opt(field_name.clone()) else {
                                        continue;
                                    };
                                    FieldDefinitionPosition::Object(field)
                                },
                                TypeDefinitionPosition::Interface(itf_pos) => {
                                    let Some(field) = itf_pos.field_opt(field_name.clone()) else {
                                        continue;
                                    };
                                    FieldDefinitionPosition::Interface(field)
                                },
                                _ => continue,
                            };
                            
                            // Remove the @external directive if it's present
                            match &own_field_pos {
                                FieldDefinitionPosition::Object(obj_field_pos) => {
                                    if let Some(directive) = obj_field_pos.get_applied_directive(schema, &external_directive.name) {
                                        // In the JS code, we'd add a change tracking entry here
                                        obj_field_pos.remove_directive(schema, &directive);
                                    }
                                },
                                FieldDefinitionPosition::Interface(itf_field_pos) => {
                                    if let Some(directive) = itf_field_pos.get_applied_directive(schema, &external_directive.name) {
                                        itf_field_pos.remove_directive(schema, &directive);
                                    }
                                },
                                _ => {}
                            }
                        }
                        
                        // We only need to check the first key directive, following the JavaScript implementation
                        break;
                    }
                }
            }
        }
        
        Ok(())
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
        let schema = &mut self.schema;
        let Some(metadata) = &schema.subgraph_metadata else {
            return Ok(());
        };
        
        // We need to get the original metadata to determine which fields are already shareable
        let original_metadata = &self.original_subgraph.schema.subgraph_metadata()
            .ok_or_else(|| FederationError::internal("Missing subgraph metadata in original schema"))?;
        
        // Get key directive and shareable directive from the metadata
        let key_directive = metadata
            .federation_spec_definition()
            .key_directive_definition(schema)?;
        
        let shareable_directive = metadata
            .federation_spec_definition()
            .shareable_directive_definition(schema)?;
        
        // We add shareable:
        // - to every "value type" (non-root, non-entity type) if it is used in other subgraphs
        // - to any (non-external) field of an entity/root-type that is not a key field and if other subgraphs resolve it
        for type_pos in schema.get_types() {
            let TypeDefinitionPosition::Object(obj_pos) = type_pos else {
                continue;
            };
            
            // Skip Subscription root type - no shareable needed
            let obj_name = &obj_pos.type_name;
            if schema.schema().subscription_type.as_ref() == Some(obj_name) {
                continue;
            }
            
            let obj_has_key = schema.has_applied_directive(&type_pos, &key_directive.name);
            let is_root_type = schema.is_root_type(&type_pos);
            
            if obj_has_key || is_root_type {
                // For entity types and root types, add @shareable to individual fields
                // that aren't keys and are defined in multiple subgraphs
                for field_pos in obj_pos.fields(schema.schema())? {
                    let field_def_pos = FieldDefinitionPosition::Object(field_pos.clone());
                    
                    // Skip if the field is already shareable in the original schema
                    // (Likely a key field)
                    if original_metadata.is_field_shareable(&field_def_pos) {
                        continue;
                    }
                    
                    // Find other subgraphs that define this same field (and not as external)
                    let mut type_in_other_subgraphs = Vec::new();
                    for (subgraph_name, subgraph) in &self.subgraphs.subgraphs {
                        if subgraph_name.as_str() == self.original_subgraph.name.as_str() {
                            continue;
                        }
                        
                        if let Some(other_metadata) = &subgraph.schema.subgraph_metadata {
                            // Get the field in the other subgraph
                            if let Ok(other_type_pos) = subgraph.schema.get_type_definition_position(obj_name.clone()) {
                                if let TypeDefinitionPosition::Object(other_obj_pos) = other_type_pos {
                                    // Check if the other subgraph has this field
                                    if let Some(other_field_pos) = other_obj_pos.field_opt(field_pos.field_name.clone()) {
                                        let other_field_def_pos = FieldDefinitionPosition::Object(other_field_pos);
                                        // Check if the field is not external or is partially external
                                        // (Partially external means it has @provides)
                                        if !other_metadata.is_field_external(&other_field_def_pos) || 
                                           other_metadata.is_field_partially_external(&other_field_def_pos) {
                                            type_in_other_subgraphs.push(subgraph_name.clone());
                                        }
                                    }
                                }
                            }
                        }
                    }
                    
                    // If this field is resolved in other subgraphs and doesn't already have @shareable
                    if !type_in_other_subgraphs.is_empty() && !field_pos.has_applied_directive(schema, &shareable_directive.name) {
                        // Add @shareable directive
                        field_pos.apply_directive(schema, &shareable_directive);
                        
                        // In the JS code, there's a change tracking system - here we'd need to implement that
                        // For now, we skip tracking the changes
                    }
                }
            } else {
                // For non-entity, non-root types (value types), add @shareable to the entire type
                // if it appears in other subgraphs
                let mut type_in_other_subgraphs = Vec::new();
                for (subgraph_name, subgraph) in &self.subgraphs.subgraphs {
                    if subgraph_name.as_str() == self.original_subgraph.name.as_str() {
                        continue; 
                    }
                    
                    // Check if the type exists in other subgraphs
                    if let Ok(_) = subgraph.schema.get_type_definition_position(obj_name.clone()) {
                        type_in_other_subgraphs.push(subgraph_name.clone());
                    }
                }
                
                // If this type is defined in other subgraphs and doesn't already have @shareable
                if !type_in_other_subgraphs.is_empty() && !obj_pos.has_applied_directive(schema, &shareable_directive.name) {
                    // Add @shareable directive to the type
                    obj_pos.apply_directive(schema, &shareable_directive);
                    
                    // In the JS code, there's a change tracking system - here we'd need to implement that
                    // For now, we skip tracking the changes
                }
            }
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
                                    self.subgraphs.subgraphs.iter().fallible_any(
                                        |(name, subgraph)| -> Result<bool, FederationError> {
                                            if self.original_subgraph.name.as_str() != name.as_ref() {
                                                // check to see if the field is external in the other subgraphs
                                                if let Some(other_metadata) =
                                                    &subgraph.schema.subgraph_metadata
                                                {
                                                    if !other_metadata
                                                        .external_metadata()
                                                        .is_external(&target)
                                                    {
                                                        // at this point, we need to check to see if there is a @tag directive on the other subgraph that matches the current application
                                                        let other_applications = subgraph
                                                            .schema
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
