use std::slice::Iter;

use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::Node;

use crate::merge::DirectiveNames;
use crate::merge::Merger;

pub(super) fn merge_arguments(
    arguments: Iter<Node<InputValueDefinition>>,
    arguments_to_merge: &mut Vec<Node<InputValueDefinition>>,
    merger: &mut Merger,
    directive_names: &DirectiveNames,
) {
    for arg in arguments {
        let argument_to_merge = arguments_to_merge
            .iter_mut()
            .find_map(|a| (a.name == arg.name).then(|| a.make_mut()));

        if let Some(argument) = argument_to_merge {
            merger.add_inaccessible(directive_names, &mut argument.directives, &arg.directives);
        } else {
            let mut argument = InputValueDefinition {
                name: arg.name.clone(),
                description: arg.description.clone(),
                directives: Default::default(),
                ty: arg.ty.clone(),
                default_value: arg.default_value.clone(),
            };

            merger.add_inaccessible(directive_names, &mut argument.directives, &arg.directives);
            arguments_to_merge.push(argument.into());
        };
    }
}
