use apollo_compiler::ast;
use apollo_compiler::ast::Name;
use apollo_compiler::ast::VariableDefinition;
use apollo_compiler::executable;
use apollo_compiler::executable::Operation;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::validation::Valid;
use apollo_compiler::validation::WithErrors;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Node;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use std::collections::HashMap;
use std::collections::HashSet;

use super::fetch::SubgraphOperation;
use super::rewrites::DataKeyRenamer;
use super::rewrites::DataRewrite;

use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::spec::Schema;

#[derive(Debug)]
pub(crate) struct ContextualArguments {
    pub(crate) arguments: HashSet<String>, // a set of all argument names that will be passed to the subgraph. This is the unmodified name from the query plan
    pub(crate) count: usize, // the number of different sets of arguments that exist. This will either be 1 or the number of entities
}

pub(crate) struct SubgraphContext<'a> {
    pub(crate) data: &'a Value,
    pub(crate) schema: &'a Schema,
    pub(crate) context_rewrites: &'a Vec<DataRewrite>,
    pub(crate) named_args: Vec<HashMap<String, Value>>,
}

// context_path is a non-standard relative path which may navigate up the tree
// from the current position. This is indicated with a ".." PathElement::Key
// note that the return value is an absolute path that may be used anywhere
fn merge_context_path(
    current_dir: &Path,
    context_path: &Path,
) -> Result<Path, ContextBatchingError> {
    let mut i = 0;
    let mut j = current_dir.len();
    // iterate over the context_path(i), every time we encounter a '..', we want
    // to go up one level in the current_dir(j)
    while i < context_path.len() {
        match &context_path.0.get(i) {
            Some(PathElement::Key(e, _)) => {
                let mut found = false;
                if e == ".." {
                    while !found {
                        if j == 0 {
                            return Err(ContextBatchingError::InvalidRelativePath);
                        }
                        j -= 1;

                        if let Some(PathElement::Key(_, _)) = current_dir.0.get(j) {
                            found = true;
                        }
                    }
                    i += 1;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }

    let mut return_path: Vec<PathElement> = current_dir.iter().take(j).cloned().collect();

    context_path.iter().skip(i).for_each(|e| {
        return_path.push(e.clone());
    });
    Ok(Path(return_path.into_iter().collect()))
}

impl<'a> SubgraphContext<'a> {
    pub(crate) fn new(
        data: &'a Value,
        schema: &'a Schema,
        context_rewrites: &'a Option<Vec<DataRewrite>>,
    ) -> Option<SubgraphContext<'a>> {
        if let Some(rewrites) = context_rewrites {
            if rewrites.len() > 0 {
                return Some(SubgraphContext {
                    data,
                    schema,
                    context_rewrites: rewrites,
                    named_args: Vec::new(),
                });
            }
        }
        None
    }

    // For each of the rewrites, start collecting data for the data at path.
    // Once we find a Value for a given variable, skip additional rewrites that
    // reference the same variable
    pub(crate) fn execute_on_path(&mut self, path: &Path) {
        let mut found_rewrites: HashSet<String> = HashSet::new();
        let hash_map: HashMap<String, Value> = self
            .context_rewrites
            .iter()
            .filter_map(|rewrite| {
                match rewrite {
                    DataRewrite::KeyRenamer(item) => {
                        if !found_rewrites.contains(item.rename_key_to.as_str()) {
                            let wrapped_data_path = merge_context_path(path, &item.path);
                            if let Ok(data_path) = wrapped_data_path {
                                let val = self.data.get_path(self.schema, &data_path);

                                if let Ok(v) = val {
                                    // add to found
                                    found_rewrites.insert(item.rename_key_to.clone().to_string());
                                    // TODO: not great
                                    let mut new_value = v.clone();
                                    if let Some(values) = new_value.as_array_mut() {
                                        for v in values {
                                            let data_rewrite = DataRewrite::KeyRenamer({
                                                DataKeyRenamer {
                                                    path: data_path.clone(),
                                                    rename_key_to: item.rename_key_to.clone(),
                                                }
                                            });
                                            data_rewrite.maybe_apply(self.schema, v);
                                        }
                                    } else {
                                        let data_rewrite = DataRewrite::KeyRenamer({
                                            DataKeyRenamer {
                                                path: data_path.clone(),
                                                rename_key_to: item.rename_key_to.clone(),
                                            }
                                        });
                                        data_rewrite.maybe_apply(self.schema, &mut new_value);
                                    }
                                    return Some((item.rename_key_to.to_string(), new_value));
                                }
                            }
                        }
                        None
                    }
                    DataRewrite::ValueSetter(_) => None,
                }
            })
            .collect();
        self.named_args.push(hash_map);
    }

    // Once all a value has been extracted for every variable, go ahead and add all
    // variables to the variables map. Additionally, return a ContextualArguments structure if
    // values of variables are entity dependent
    pub(crate) fn add_variables_and_get_args(
        &self,
        variables: &mut Map<ByteString, Value>,
    ) -> Option<ContextualArguments> {
        let (extended_vars, contextual_args) = if let Some(first_map) = self.named_args.first() {
            if self.named_args.iter().all(|map| map == first_map) {
                (
                    first_map
                        .iter()
                        .map(|(k, v)| (k.as_str().into(), v.clone()))
                        .collect(),
                    None,
                )
            } else {
                let mut hash_map: HashMap<String, Value> = HashMap::new();
                let arg_names: HashSet<_> = first_map.keys().cloned().collect();
                for (index, item) in self.named_args.iter().enumerate() {
                    // append _<index> to each of the arguments and push all the values into hash_map
                    hash_map.extend(item.iter().map(|(k, v)| {
                        let mut new_named_param = k.clone();
                        new_named_param.push_str(&format!("_{}", index));
                        (new_named_param, v.clone())
                    }));
                }
                (
                    hash_map,
                    Some(ContextualArguments {
                        arguments: arg_names,
                        count: self.named_args.len(),
                    }),
                )
            }
        } else {
            (HashMap::new(), None)
        };

        variables.extend(
            extended_vars
                .iter()
                .map(|(key, value)| (key.as_str().into(), value.clone())),
        );

        contextual_args
    }
}

// Take the existing subgraph operation and rewrite it to use aliasing. This will occur in the case
// where we are collecting entites and different entities may have different variables passed to the resolver.
pub(crate) fn build_operation_with_aliasing(
    subgraph_operation: &SubgraphOperation,
    contextual_arguments: &ContextualArguments,
    schema: &Schema,
) -> Result<Valid<ExecutableDocument>, ContextBatchingError> {
    let mut selections: Vec<Selection> = vec![];
    let ContextualArguments { arguments, count } = contextual_arguments;
    let parsed_document =
        subgraph_operation.as_parsed(schema.federation_supergraph().schema.schema());
    if let Ok(document) = parsed_document {
        // TODO: Can there be more than one named operation?
        //       Can there be an anonymous operation?
        if let Some((_, op)) = document.named_operations.first() {
            let mut new_variables: Vec<Node<VariableDefinition>> = vec![];
            op.variables.iter().for_each(|v| {
                if arguments.contains(v.name.as_str()) {
                    for i in 0..*count {
                        new_variables.push(Node::new(VariableDefinition {
                            name: Name::new_unchecked(format!("{}_{}", v.name.as_str(), i).into()),
                            ty: v.ty.clone(),
                            default_value: v.default_value.clone(),
                            directives: v.directives.clone(),
                        }));
                    }
                } else {
                    new_variables.push(v.clone());
                }
            });

            for i in 0..*count {
                // If we are aliasing, we know that there is only one selection in the top level SelectionSet
                // it is a field selection for _entities, so it's ok to reach in and give it an alias
                let mut selection_set = op.selection_set.clone();
                transform_selection_set(&mut selection_set, arguments, i, true);
                selections.push(selection_set.selections[0].clone())
            }

            let mut ed = ExecutableDocument::new();
            ed.insert_operation(Operation {
                operation_type: op.operation_type.clone(),
                name: op.name.clone(),
                directives: op.directives.clone(),
                variables: new_variables,
                selection_set: SelectionSet {
                    ty: op.selection_set.ty.clone(),
                    selections,
                },
            });

            let valid_document = ed
                .validate(schema.federation_supergraph().schema.schema())
                .map_err(ContextBatchingError::InvalidDocumentGenerated)?;

            return Ok(valid_document);
        }
    }
    Err(ContextBatchingError::NoSelectionSet)
}

// adds an alias that aligns with the index for this selection
fn add_alias_to_selection(selection: &mut executable::Field, index: usize) {
    selection.alias = Some(Name::new_unchecked(format!("_{}", index).into()));
}

// This function will take the selection set (which has been cloned from the original)
// and transform it so that all contextual variables in the selection set will be appended with a _<index>
// to match the index in the alias that it is
fn transform_selection_set(
    selection_set: &mut SelectionSet,
    arguments: &HashSet<String>,
    index: usize,
    add_alias: bool, // at the top level, we'll add an alias to field selections
) {
    selection_set
        .selections
        .iter_mut()
        .for_each(|selection| match selection {
            executable::Selection::Field(node) => {
                let node = node.make_mut();
                transform_field_arguments(&mut node.arguments, arguments, index);
                transform_selection_set(&mut node.selection_set, arguments, index, false);
                if add_alias {
                    add_alias_to_selection(node, index);
                }
            }
            executable::Selection::InlineFragment(node) => {
                let node = node.make_mut();
                transform_selection_set(&mut node.selection_set, arguments, index, false);
            }
            _ => (),
        });
}

// transforms the variable name on the field argment
fn transform_field_arguments(
    arguments_in_selection: &mut Vec<Node<ast::Argument>>,
    arguments: &HashSet<String>,
    index: usize,
) {
    arguments_in_selection.iter_mut().for_each(|arg| {
        let arg = arg.make_mut();
        if let Some(v) = arg.value.as_variable() {
            if arguments.contains(v.as_str()) {
                arg.value = Node::new(ast::Value::Variable(Name::new_unchecked(
                    format!("{}_{}", v.as_str(), index).into(),
                )));
            }
        }
    });
}

#[derive(Debug)]
pub(crate) enum ContextBatchingError {
    NoSelectionSet,
    InvalidDocumentGenerated(WithErrors<ExecutableDocument>),
    InvalidRelativePath,
}

#[cfg(test)]
mod subgraph_context_unit_tests {
    use super::*;

    #[test]
    fn test_merge_context_path() {
        let current_dir: Path = serde_json::from_str(r#"["t","u"]"#).unwrap();
        let relative_path: Path = serde_json::from_str(r#"["..","... on T","prop"]"#).unwrap();
        let expected = r#"["t","... on T","prop"]"#;

        let result = merge_context_path(&current_dir, &relative_path).unwrap();
        assert_eq!(expected, serde_json::to_string(&result).unwrap(),);
    }
    
    #[test]
    fn test_merge_context_path_invalid() {
        let current_dir: Path = serde_json::from_str(r#"["t","u"]"#).unwrap();
        let relative_path: Path =
            serde_json::from_str(r#"["..","..","..","... on T","prop"]"#).unwrap();

        let result = merge_context_path(&current_dir, &relative_path);
        match result {
            Ok(_) => panic!("Expected an error, but got Ok"),
            Err(e) => match e {
                ContextBatchingError::InvalidRelativePath => (),
                _ => panic!("Expected InvalidRelativePath, but got a different error"),
            },
        }
    }

    // #[test]
    // fn test_merge_context_path() {
    //     let old_query = "query QueryLL__Subgraph1__1($representations:[_Any!]!$Subgraph1_U_field_a:String!){_entities(representations:$representations){...on U{id field(a:$Subgraph1_U_field_a)}}}";
    //     let mut ctx_args = HashSet::new();
    //     ctx_args.insert("Subgraph1_U_field_a".to_string());
    //     let contextual_args = Some((ctx_args, 2));

    //     let expected = "query QueryLL__Subgraph1__1($representations: [_Any!]!, $Subgraph1_U_field_a_0: String!, $Subgraph1_U_field_a_1: String!) { _0: _entities(representations: $representations) { ... on U { id field(a: $Subgraph1_U_field_a_0) } } _1: _entities(representations: $representations) { ... on U { id field(a: $Subgraph1_U_field_a_1) } } }";

    //     assert_eq!(
    //         expected,
    //         build_operation_with_aliasing(old_query, &contextual_args)
    //             .unwrap()
    //             .serialize()
    //             .no_indent()
    //             .to_string()
    //             .as_str()
    //     );
    // }
}
