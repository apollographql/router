use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Value;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::Name;
use apollo_compiler::Node;

use crate::error::CompositionError;
use crate::error::FederationError;
use crate::link::join_spec_definition::TypeDirectiveArguments;
use crate::link::join_spec_definition::JOIN_EXTENSION_ARGUMENT_NAME;
use crate::link::join_spec_definition::JOIN_GRAPH_DIRECTIVE_NAME_IN_SPEC;
use crate::link::join_spec_definition::JOIN_ISINTERFACEOBJECT_ARGUMENT_NAME;
use crate::link::join_spec_definition::JOIN_KEY_ARGUMENT_NAME;
use crate::link::join_spec_definition::JOIN_RESOLVABLE_ARGUMENT_NAME;
use crate::schema::position::TypeDefinitionPosition;
use crate::schema::SchemaElement;
use crate::merger::merge::Merger;
use crate::merger::merge::Sources;

impl Merger {
    fn check_for_extension_with_no_base(
        &mut self,
        sources: &Sources<ExtendedType>,
        dest: &ExtendedType,
    ) {
        if let ExtendedType::Object(obj) = dest {
            if self.merged.is_root_type(&obj.name) {
                return;
            }
        }
        
        let mut def_subgraphs: Vec<String> = Vec::new();
        let mut extension_subgraphs: Vec<String> = Vec::new();
        for (idx, source) in sources.iter() {
            let Some(source) = source else {
                continue;
            };
            if source.has_non_extension_elements() {
                def_subgraphs.push(self.names[*idx].clone());
            }
            if source.has_extension_elements() {
                extension_subgraphs.push(self.names[*idx].clone());
            }
        }

        if !extension_subgraphs.is_empty() && def_subgraphs.is_empty() {
            for subgraph in extension_subgraphs {
                self.error_reporter.add_error(CompositionError::ExtensionWithNoBase {
                    message: format!("{} Type {} is an extension type, but this is no type definition for {} in any subgraph.", subgraph, dest.name(), dest.name())
                });
                // TODO: Add AST to error
            }
        }
        
    }
        
    fn add_join_type(
        &mut self,
        sources: &Sources<ExtendedType>,
        dest: &ExtendedType,
    ) -> Result<(), FederationError> {
        let dest_pos: TypeDefinitionPosition = TypeDefinitionPosition::from(dest);
        let join_type_name = self.join_spec_definition.join_type_definition(&self.merged)?.name.clone();
        for (idx, source) in sources.iter() {
            let Some(source) = source else {
                continue;
            };
            let subgraph = &self.subgraphs[*idx];
            let source_pos = TypeDefinitionPosition::from(source);
            let is_interface_object = subgraph.is_interface_object_type(&source_pos);
            let subgraph_name = self.join_spec_name(*idx)?.clone();
            let key_directive_name = subgraph.key_directive_name()?;
            let Some(key_directive_name) = key_directive_name else {
                continue;
            };
            
            let key_directives = source_pos.get_applied_directives(subgraph.schema(), &key_directive_name);
            if key_directives.is_empty() {
                let directive = create_join_type_directive(&join_type_name, &TypeDirectiveArguments {
                    graph: subgraph_name.clone(),
                    key: None,
                    extension: false,
                    resolvable: true,
                    is_interface_object: is_interface_object,
                });
                dest_pos.insert_directive(&mut self.merged, directive)?;
            } else {
                for key_directive in key_directives {
                    let key_arguments = subgraph.metadata().federation_spec_definition().key_directive_arguments(key_directive)?;
                    let directive = create_join_type_directive(&join_type_name, &TypeDirectiveArguments {
                        graph: subgraph_name.clone(),
                        key: Some(key_arguments.fields),
                        extension: false, // TODO: Check this
                        resolvable: key_arguments.resolvable,
                        is_interface_object: is_interface_object,
                    });
                    dest_pos.insert_directive(&mut self.merged, directive)?;
                }
            }
        }
        Ok(())
    }
}

fn create_join_type_directive(
    name: &Name,
    arguments: &TypeDirectiveArguments,
) -> Component<Directive> {
    let mut args = vec![
            Node::new(Argument {
                name: JOIN_GRAPH_DIRECTIVE_NAME_IN_SPEC,
                value: Node::new(Value::Enum(arguments.graph.clone())),
            })
        ];
    
    if let Some(key) = arguments.key {
        args.push(Node::new(Argument {
            name: JOIN_KEY_ARGUMENT_NAME,
            value: Node::new(Value::String(key.to_string())),
        }));
    }
    if arguments.extension {
        args.push(Node::new(Argument {
            name: JOIN_EXTENSION_ARGUMENT_NAME,
            value: Node::new(Value::Boolean(arguments.extension)),
        }));
    }
    if !arguments.resolvable {
        args.push(Node::new(Argument {
            name: JOIN_RESOLVABLE_ARGUMENT_NAME,
            value: Node::new(Value::Boolean(arguments.resolvable)),
        }));        
    }
    if arguments.is_interface_object {
        args.push(Node::new(Argument {
            name: JOIN_ISINTERFACEOBJECT_ARGUMENT_NAME,
            value: Node::new(Value::Boolean(arguments.is_interface_object)),
        }));        
    }
    Component::new(Directive {
        name: name.clone(),
        arguments: args,
    })
}
