use apollo_compiler::name;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Value;
use apollo_compiler::collections::HashMap;
use apollo_compiler::collections::HashSet;

use crate::bail;
use crate::error::FederationError;
use crate::link::join_spec_definition::JOIN_DIRECTIVE_DIRECTIVE_NAME_IN_SPEC;
use crate::link::join_spec_definition::JOIN_NAME_ARGUMENT_NAME;
use crate::link::link_spec_definition::LinkDirectiveArguments;
use crate::link::spec::Url;
use crate::merger::merge::Merger;
use crate::merger::merge::Sources;
use crate::schema::position::DirectiveTargetPosition;

use serde_json;

pub(crate) struct AppliedDirectivesToMerge {
    names: HashSet<Name>,
    sources: Sources<DirectiveTargetPosition>,
    dest: DirectiveTargetPosition,
}

#[derive(Debug, Clone)]
struct JoinDirectiveGroup {
    /// List of subgraph names that have this directive application
    graphs: Vec<String>,
    /// Serialized directive arguments for comparison
    args: HashMap<String, String>,
}

impl Merger {
    pub(crate) fn record_applied_directives_to_merge(
        &mut self,
        sources: &Sources<DirectiveTargetPosition>,
        dest: &DirectiveTargetPosition,
    ) -> Result<(), FederationError> {
        // let mut names = self.gather_applied_directive_names(sources);
        let mut names: HashSet<Name> = Default::default();
        if let Some(inaccessible_name) = &self.inaccessible_directive_name_in_supergraph {
            names.remove(inaccessible_name);
            self.merge_applied_directive(&inaccessible_name.clone(), sources)?;
        }
        
        // each DirectiveTargetPosition will be the same, but these objects are lightweight and cheap to clone
        let source_positions: Sources<DirectiveTargetPosition> = sources
            .iter()
            .map(|(&idx, src)| (idx, src.clone()))
            .collect();

        self.applied_directives_to_merge
            .push(AppliedDirectivesToMerge {
                names,
                sources: source_positions,
                dest: dest.clone(),
            });

        Ok(())
    }
    
    /// Gather applied directive names from all sources (ported from JavaScript gatherAppliedDirectiveNames())
    fn gather_applied_directive_names(
        &self,
        sources: &Sources<DirectiveTargetPosition>,
    ) -> HashSet<Name> {
        let mut names: HashSet<Name> = Default::default();

        for (&idx, source) in sources.iter() {
            if let Some(source) = source {
                let schema = self.subgraphs[idx].schema();
                for directive in source.get_all_applied_directives(schema) {
                    if self.is_merged_directive(&self.names[idx], &directive.name) {
                        names.insert(directive.name.clone());
                    }
                }
            }
        }

        names
    }
    
    /// Add join directive directives (ported from JavaScript addJoinDirectiveDirectives())
    pub(crate) fn add_join_directive_directives(
        &mut self,
        sources: &Sources<DirectiveTargetPosition>,
        dest: DirectiveTargetPosition,
    ) -> Result<(), FederationError> {
        // This method handles the reflection of subgraph directive applications in the supergraph
        // using @join__directive(graphs, name, args) directives.
        // Map to group directives by name and arguments: directive_name -> Vec<(graphs, args)>
        let mut joins_by_directive_name: HashMap<String, Vec<JoinDirectiveGroup>> =
            Default::default();
        let mut links_to_persist: Vec<LinkDirectiveArguments> = Vec::new();
        
        // Collect directive applications from all sources
        for (&idx, source) in sources.iter() {
            let Some(source) = source else {
                continue;
            };
            let subgraph_name = self.join_spec_name(idx)?;
            let subgraph_schema = self.subgraphs[idx].schema();
            let Some(link_import_identity_url_map) = self.schema_to_import_to_feature_url.get(subgraph_name.as_str()).cloned() else {
                continue;
            };
            
            // Get all directives applied to this source type
            for directive in source.get_all_applied_directives(subgraph_schema) {
                // Check if this directive should be represented as a join directive and process it
                self.should_use_join_directive_for_directive(
                    directive,
                    &subgraph_name,
                    &link_import_identity_url_map,
                    &mut joins_by_directive_name,
                    &mut links_to_persist,
                )?;
            }
        }
        
        // Apply @join__directive directives to the destination
        for (directive_name, groups) in joins_by_directive_name {
            for group in groups {
                self.apply_join_directive_directive(&dest, &directive_name, &group)?;
            }
        }
        Ok(())
    }
    
    /// Helper function to apply a single @join__directive directive
    fn apply_join_directive_directive(
        &self,
        _dest: &DirectiveTargetPosition,
        directive_name: &str,
        group: &JoinDirectiveGroup,
    ) -> Result<(), FederationError> {
        let mut arguments = Vec::new();
        // graphs argument (required) - List of subgraph enum values
        let graph_values: Vec<Node<Value>> = group
            .graphs
            .iter()
            .map(|graph_name| {
                let graph_enum_name = Name::new(graph_name)?;
                Ok(Node::new(Value::Enum(graph_enum_name)))
            })
            .collect::<Result<Vec<_>, FederationError>>()?;
        arguments.push(Node::new(Argument {
            name: name!("graphs"),
            value: Value::List(graph_values).into(),
        }));
        // name argument (required) - The directive name
        arguments.push(Node::new(Argument {
            name: JOIN_NAME_ARGUMENT_NAME,
            value: Value::String(directive_name.to_string()).into(),
        }));
        // args argument (optional) - The directive arguments if any
        if !group.args.is_empty() {
            // TODO: Serialize the args map to DirectiveArguments scalar format
            // For now, we'll use a simple JSON-like string representation
            let args_string = serde_json::to_string(&group.args).map_err(|e| {
                FederationError::internal(format!("Failed to serialize args: {}", e))
            })?;
            arguments.push(Node::new(Argument {
                name: name!("args"),
                value: Value::String(args_string).into(),
            }));
        }
        // Create and apply the @join__directive directive
        let _directive = Directive {
            name: JOIN_DIRECTIVE_DIRECTIVE_NAME_IN_SPEC,
            arguments,
        };
        // Apply the directive to the destination type
        // Note: This would need to be implemented based on how ExtendedType supports adding directives
        // For now, this is a placeholder that would need proper implementation
        todo!("Implement directive application to ExtendedType");
    }
    
    /// Check if a directive should be represented as a join directive and process it if so
    fn should_use_join_directive_for_directive(
        &self,
        directive: &Node<Directive>,
        subgraph_name: &str,
        link_import_identity_url_map: &std::collections::HashMap<String, Url>,
        joins_by_directive_name: &mut HashMap<String, Vec<JoinDirectiveGroup>>,
        links_to_persist: &mut Vec<LinkDirectiveArguments>,
    ) -> Result<(), FederationError> {
        let Some(metadata) = self.merged.metadata() else {
            bail!("No metadata found");
        };
        
        let should_use_join_directive = if directive.name.as_str() == "link" {
            let args = metadata.link_spec_definition()?.link_directive_arguments(directive)?;
            args.url.parse::<Url>()
                .ok()
                .map(|parsed_url| {
                    let should_use = self.should_use_join_directive_for_url(&parsed_url);
                    if should_use {
                        links_to_persist.push(args);
                    }
                    should_use
                })
                .unwrap_or(false)
        } else {
            // For non-link directives, look up the directive name in the import map
            link_import_identity_url_map
                .get(directive.name.as_str())
                .map(|url| self.should_use_join_directive_for_url(&url))
                .unwrap_or(false)
        };
        
        // Skip federation directives that shouldn't use join directive
        let should_skip = matches!(directive.name.as_str(), 
            "key" | "requires" | "provides" | "external" | "extends" | "shareable" | "override"
            | "inaccessible" | "tag" | "interfaceObject" | "composeDirective"
        );
        
        if should_use_join_directive && !should_skip {
            // Convert directive arguments to a serializable format for grouping
            let args_map = self.serialize_directive_arguments(&directive.arguments)?;
            let directive_name = directive.name.as_str().to_string();
            
            // Find or create the group for this directive name
            let directive_groups = joins_by_directive_name
                .entry(directive_name)
                .or_insert_with(Vec::new);
            
            // Look for an existing group with the same arguments
            if let Some(existing_group) = directive_groups.iter_mut().find(|group| group.args == args_map) {
                // Add this subgraph to the existing group
                existing_group.graphs.push(subgraph_name.to_string());
            } else {
                // Create a new group for this directive application
                directive_groups.push(JoinDirectiveGroup {
                    graphs: vec![subgraph_name.to_string()],
                    args: args_map,
                });
            }
        }
        
        Ok(())
    }
    
    fn should_use_join_directive_for_url(
        &self,
        _url: &Url,
    ) -> bool {
        // For now, assume all URLs should use join directive
        // This logic may need to be refined based on specific URL patterns
        true
    }
    
    /// Serialize directive arguments to a HashMap for comparison
    fn serialize_directive_arguments(
        &self,
        arguments: &Vec<Node<Argument>>,
    ) -> Result<HashMap<String, String>, FederationError> {
        let mut args_map: HashMap<String, String> = Default::default();
        for arg in arguments {
            let value_str = match &*arg.value {
                Value::String(s) => s.clone(),
                Value::Int(i) => i.to_string(),
                Value::Float(f) => f.to_string(),
                Value::Boolean(b) => b.to_string(),
                Value::Null => "null".to_string(),
                Value::Enum(e) => e.to_string(),
                Value::List(_) => "[list]".to_string(), // Simplified for now
                Value::Object(_) => "{object}".to_string(), // Simplified for now
                Value::Variable(name) => format!("${}", name), // Variable reference
            };
            args_map.insert(arg.name.to_string(), value_str);
        }
        Ok(args_map)
    }
}
