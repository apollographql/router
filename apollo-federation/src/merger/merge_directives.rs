use apollo_compiler::{
    Name, Node,
    ast::{Argument, Directive, Value},
    collections::{HashMap, HashSet},
    name,
    schema::{Component, ExtendedType},
};

use crate::{
    bail, error::{CompositionError, FederationError}, link::{link_spec_definition::LINK_DIRECTIVE_URL_ARGUMENT_NAME, spec::Url}, merger::merge::{Merger, Sources}, schema::position::TypeDefinitionPosition
};

pub(crate) struct AppliedDirectivesToMerge {
    names: HashSet<Name>,
    sources: Sources<TypeDefinitionPosition>,
    dest: TypeDefinitionPosition,
}

#[derive(Debug, Clone)]
struct JoinDirectiveGroup {
    /// List of subgraph names that have this directive application
    graphs: Vec<String>,
    /// Serialized directive arguments for comparison
    args: HashMap<String, String>,
}

impl Merger {
    pub(crate) fn merge_applied_directive<T>(
        &mut self,
        _name: &str,
        _sources: &Sources<T>,
        _dest: &T,
    ) -> Result<(), FederationError> {
        todo!();
    }
    
    fn record_applied_directives_to_merge(
        &mut self,
        sources: &Sources<ExtendedType>,
        dest: &ExtendedType,
    ) -> Result<(), FederationError> {
        let mut names = self.gather_applied_directive_names(sources);
        if let Some(inaccessible_name) = &self.inaccessible_directive_name_in_supergraph {
            names.remove(inaccessible_name);
            self.merge_applied_directive(inaccessible_name.to_string().as_str(), sources, dest)?;
        }
        
        // each TypeDefinitionPosition will be the same, but these objects are lightweight and cheap to clone
        let source_positions: Sources<TypeDefinitionPosition> = sources
            .iter()
            .map(|(&idx, src)| (idx, src.as_ref().map(|s| TypeDefinitionPosition::from(s))))
            .collect();

        self.applied_directives_to_merge
            .push(AppliedDirectivesToMerge {
                names,
                sources: source_positions,
                dest: dest.into(),
            });

        Ok(())
    }

    /// Gather applied directive names from all sources (ported from JavaScript gatherAppliedDirectiveNames())
    fn gather_applied_directive_names(
        &self,
        sources: &Sources<ExtendedType>,
    ) -> HashSet<Name> {
        let mut names: HashSet<Name> = Default::default();

        for (&idx, source) in sources.iter() {
            if let Some(source) = source {
                for directive in source.directives() {
                    if self.is_merged_directive(&self.names[idx], directive) {
                        names.insert(directive.name.clone());
                    }
                }
            }
        }

        names
    }

    /// Add join directive directives (ported from JavaScript addJoinDirectiveDirectives())
    fn add_join_directive_directives(
        &mut self,
        sources: &Sources<&ExtendedType>,
        dest: &mut ExtendedType,
    ) -> Result<(), FederationError> {
        // This method handles the reflection of subgraph directive applications in the supergraph
        // using @join__directive(graphs, name, args) directives.
        // Map to group directives by name and arguments: directive_name -> Vec<(graphs, args)>
        let mut joins_by_directive_name: HashMap<String, Vec<JoinDirectiveGroup>> =
            Default::default();
        // Collect directive applications from all sources
        for (&idx, source) in sources.iter() {
            let Some(source) = *source else {
                continue;
            };
            let subgraph_name = self.join_spec_name(idx)?;
            // Get all directives applied to this source type
            for directive in source.directives() {
                // Check if this directive should be represented as a join directive
                if self.should_use_join_directive_for_directive(directive) {
                    // Convert directive arguments to a serializable format
                    let args = &directive.arguments;
                    // Find or create the group for this directive name
                    let directive_groups = joins_by_directive_name
                        .entry(directive_name.clone())
                        .or_insert_with(Vec::new);
                    // Look for an existing group with the same arguments
                    if let Some(existing_group) =
                        directive_groups.iter_mut().find(|group| group.args == args)
                    {
                        // Add this subgraph to the existing group
                        existing_group.graphs.push(join_spec_name.to_string());
                    } else {
                        // Create a new group for this directive application
                        directive_groups.push(JoinDirectiveGroup {
                            graphs: vec![join_spec_name.to_string()],
                            args,
                        });
                    }
                }
            }
        }
        // Apply @join__directive directives to the destination
        for (directive_name, groups) in joins_by_directive_name {
            for group in groups {
                self.apply_join_directive_directive(dest, &directive_name, &group)?;
            }
        }
        Ok(())
    }
    /// Helper function to apply a single @join__directive directive
    fn apply_join_directive_directive(
        &self,
        dest: &mut ExtendedType,
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
            let args_string = self.serialize_args_map(&group.args)?;
            arguments.push(Node::new(Argument {
                name: name!("args"),
                value: Value::String(args_string).into(),
            }));
        }
        // Create and apply the @join__directive directive
        let directive = Directive {
            name: JOIN_DIRECTIVE_DIRECTIVE_NAME_IN_SPEC,
            arguments,
        };
        match dest {
            ExtendedType::Scalar(scalar) => {
                scalar.make_mut().directives.push(Node::new(directive));
            }
            ExtendedType::Object(object) => {
                object.make_mut().directives.push(Node::new(directive));
            }
            ExtendedType::Interface(interface) => {
                interface.make_mut().directives.push(Node::new(directive));
            }
            ExtendedType::Union(union) => {
                union.make_mut().directives.push(Node::new(directive));
            }
            ExtendedType::Enum(enum_type) => {
                enum_type.make_mut().directives.push(Node::new(directive));
            }
            ExtendedType::InputObject(input_object) => {
                input_object
                    .make_mut()
                    .directives
                    .push(Node::new(directive));
            }
        }
        Ok(())
    }
    /// Check if a directive should be represented as a join directive (stub implementation)
    fn should_use_join_directive_for_directive(
        &self,
        directive: &Component<Directive>,
    ) -> Result<bool, FederationError> {
        let Some(_) = self.merged.metadata().map(|m| m.link_spec_definition()) else {
            bail!("No link definition found");
        };
        if directive.name.as_str() == "link" {
            let url = match directive.arguments.iter().find(|arg| arg.name == LINK_DIRECTIVE_URL_ARGUMENT_NAME).map(|arg| arg.value.as_str()).flatten() {
                Some(url) => url.parse::<Url>()?,
                None => bail!("No url argument found"),
            };
        }
        // TODO: Implement proper logic for determining which directives should use join directive representation
        // This should check:
        // 1. For @link directives: Check if the URL should use join directive representation
        // 2. For other directives: Look up in link import identity URL map
        // For now, skip common federation directives that are handled elsewhere
        match directive.name.as_str() {
            "key" | "requires" | "provides" | "external" | "extends" | "shareable" | "override"
            | "inaccessible" | "tag" | "interfaceObject" | "composeDirective" => Ok(false),
            _ => Ok(true), // Default to using join directive for other directives
        }
    }
}
