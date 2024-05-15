use std::collections::HashMap;
use std::collections::HashSet;
use apollo_compiler::ast;
use apollo_compiler::ast::Document;
use apollo_compiler::ast::Name;
use apollo_compiler::ast::VariableDefinition;
use apollo_compiler::executable;
use apollo_compiler::executable::Operation;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::validation::WithErrors;
use apollo_compiler::Node;
use apollo_compiler::NodeStr;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;

use super::fetch::SubgraphOperation;
use super::rewrites;
use super::rewrites::DataKeyRenamer;
use super::rewrites::DataRewrite;
// use crate::json_ext::Object;

use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::json_ext::Value;
// use crate::json_ext::ValueExt;
use crate::spec::Schema;

pub(crate) struct ContextualArguments {
  pub(crate) arguments: HashSet<String>, // a set of all argument names that will be passed to the subgraph. This is the unmodified name from the query plan
  pub(crate) count: usize, // the number of different sets of arguments that exist. This will either be 1 or the number of entities
}

pub(crate) struct SubgraphContext<'a> {
    pub(crate) data: &'a Value,
    pub(crate) current_dir: &'a Path,
    pub(crate) schema: &'a Schema,
    pub(crate) context_rewrites: &'a Vec<DataRewrite>,
    pub(crate) named_args: Vec<HashMap<String, Value>>,
}

// TODO: There is probably a function somewhere else that already does this
fn data_at_path<'v>(data: &'v Value, path: &Path) -> Option<&'v Value> {
  let v = match &path.0[0] {
      PathElement::Fragment(s) => {
          // get the value at data.get("__typename") and compare it with s. If the values are equal, return data, otherwise None
          let mut z: Option<&Value> = None;
          let wrapped_typename = data.get("__typename");
          if let Some(t) = wrapped_typename {
              if t.as_str() == Some(s.as_str()) {
                  z = Some(data);
              }
          }
          z
      }
      PathElement::Key(v, _) => data.get(v),
      PathElement::Index(idx) => Some(&data[idx]),
      PathElement::Flatten(_) => None,
  };

  if path.len() > 1 {
      if let Some(val) = v {
          let remaining_path = path.iter().skip(1).cloned().collect();
          return data_at_path(val, &remaining_path);
      }
  }
  v
}

fn merge_context_path(current_dir: &Path, context_path: &Path) -> Path {
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
  Path(return_path.into_iter().collect())
}


impl<'a> SubgraphContext<'a> {
    pub(crate) fn new(
        data: &'a Value,
        current_dir: &'a Path,
        schema: &'a Schema,
        context_rewrites: &'a Option<Vec<DataRewrite>>,
    ) -> Option<SubgraphContext<'a>> {
        if let Some(rewrites) = context_rewrites {
            if rewrites.len() > 0 {
                return Some(SubgraphContext {
                    data,
                    current_dir,
                    schema,
                    context_rewrites: rewrites,
                    named_args: Vec::new(),
                })
            }
        }
        None
    }
    
    pub(crate) fn execute_on_path(
        &mut self,
        path: &Path,
    ) {
        let mut found_rewrites: HashSet<String> = HashSet::new();
        let hash_map: HashMap<String, Value> = self.context_rewrites
            .iter()
            .filter_map(|rewrite| {
            match rewrite {
                DataRewrite::KeyRenamer(item) => {
                    if !found_rewrites.contains(item.rename_key_to.as_str()) {
                        let data_path = merge_context_path(path, &item.path);
                        let val = data_at_path(self.data, &data_path);
                        if let Some(v) = val {
                            // add to found
                            found_rewrites
                                .insert(item.rename_key_to.clone().to_string());
                            // TODO: not great
                            let mut new_value = v.clone();
                            if let Some(values) = new_value.as_array_mut() {
                                for v in values {
                                    rewrites::apply_single_rewrite(
                                        self.schema,
                                        v,
                                        &DataRewrite::KeyRenamer({
                                            DataKeyRenamer {
                                                path: data_path.clone(),
                                                rename_key_to: item
                                                    .rename_key_to
                                                    .clone(),
                                            }
                                        }),
                                    );
                                }
                            } else {
                                rewrites::apply_single_rewrite(
                                    self.schema,
                                    &mut new_value,
                                    &DataRewrite::KeyRenamer({
                                        DataKeyRenamer {
                                            path: data_path,
                                            rename_key_to: item.rename_key_to.clone(),
                                        }
                                    }),
                                );
                            }
                            return Some((item.rename_key_to.to_string(), new_value));
                        }
                    }
                    None
                },
                DataRewrite::ValueSetter(_) => {
                    None
                }
            }
            })
            .collect();
        self.named_args.push(hash_map);
    }
    
    pub(crate) fn add_variables_and_get_args(
        &self,
        variables: &mut Map<ByteString, Value>,
    ) -> Option<ContextualArguments> {
        dbg!(&self.named_args);
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
                (hash_map, Some(ContextualArguments{
                   arguments: arg_names,
                   count: self.named_args.len(),
                }))
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

// fn add_index_alias_to_first_selection(
//     selection_set: &SelectionSet,
//     index: usize,
// ) -> Result<Selection, ContextBatchingError> {
//     let selection = selection_set.selections.first();
//     if let Some(executable::Selection::Field(node)) = selection {
//         let z: Selection = Node::new(executable::Field {
//             definition: node.definition.clone(),
//             alias: node.alias.clone(),
//             name: node.name.clone(),
//             arguments: node.arguments.clone(),
//             directives: node.directives.clone(),
//             selection_set: node.selection_set.clone(),
//         });
//         Ok(z)
//     } else {
//         Err(ContextBatchingError::NoSelectionSet)
//     }
// }

pub fn build_operation_with_aliasing(
    subgraph_operation: &SubgraphOperation,
    contextual_arguments: &Option<ContextualArguments>,
    schema: &Schema,
) -> Result<Operation, ContextBatchingError> {
    let mut selections: Vec<Selection> = vec![];
    dbg!(subgraph_operation.as_serialized());
    match contextual_arguments {
        Some(ContextualArguments  { arguments, count }) => {
            dbg!("arguments", arguments);
            let parsed_document = subgraph_operation.as_parsed(schema.supergraph_schema());
            if let Ok(document) = parsed_document {
                // TODO: Can there be more than one named operation?
                //       Can there be an anonymous operation?
                // let mut new_variables: Vec<Node<VariableDefinition>> = vec![];
                // let mut new_selection_set: Vec<SelectionSet> = vec![];
                // let mut operations: Vec<Operation> = vec![];
                if let Some((_, op)) = document.named_operations.first() {
                    let mut new_variables: Vec<Node<VariableDefinition>> = vec![];
                    op.variables.iter().for_each(|v| {
                        if arguments.contains(v.name.as_str()) {
                            for i in 0..*count {
                                new_variables.push(
                                    Node::new(VariableDefinition {
                                        name: Name::new_unchecked(format!("{}_{}", v.name.as_str(), i).into()),
                                        ty: v.ty.clone(),
                                        default_value: v.default_value.clone(),
                                        directives: v.directives.clone(),
                                    })
                                );
                            }
                        } else {
                            new_variables.push(v.clone());
                        }
                    });
        
                    for i in 0..*count {
                        // If we are aliasing, we know that there is only one selection in the top level SelectionSet
                        // it is a field selection for _entities, so it's ok to reach in and give it an alias
                        let selection_set = transform_selection_set(&op.selection_set, arguments, i, true);
                        selections.push(selection_set.selections[0].clone())
                    };
                    
                    Ok(
                        Operation {
                            operation_type: op.operation_type.clone(),
                            name: op.name.clone(),
                            directives: op.directives.clone(),
                            variables: new_variables,
                            selection_set: SelectionSet {
                                ty: op.selection_set.ty.clone(),
                                selections,
                            },
                        }
                    )
                } else {
                    Err(ContextBatchingError::NoSelectionSet)
                }
            } else {
                Err(ContextBatchingError::NoSelectionSet)
            }
        },
        None => Err(ContextBatchingError::NoSelectionSet),
    }
}

fn transform_field_arguments(
    arguments_in_selection: &Vec<Node<ast::Argument>>,
    arguments: &HashSet<String>,
    index: usize,
) -> Vec<Node<ast::Argument>> {
    arguments_in_selection.iter().map(|arg| {
        Node::new(
            ast::Argument {
                name: if arguments.contains(arg.name.as_str()) {
                    Name::new_unchecked(format!("{}_{}", arg.value.clone(), index).into())
                } else {
                    arg.name.clone()
                },
                value: arg.value.clone(),
            }
        )
    }).collect()
}

fn transform_selection_set(
    selection_set: &SelectionSet,
    arguments: &HashSet<String>,
    index: usize,
    add_alias: bool,
) -> SelectionSet {
    SelectionSet{
        ty: selection_set.ty.clone(),
        selections: selection_set.selections.iter().map(|selection| {
            match selection {
                executable::Selection::Field(node) => {
                    executable::Selection::Field(
                        Node::new(executable::Field {
                            definition: node.definition.clone(),
                            alias: if add_alias {
                                Some(Name::new_unchecked(format!("_{}", index).into()))
                             } else {
                                node.alias.clone()
                             },
                            name: node.name.clone(),
                            arguments: transform_field_arguments(&node.arguments, arguments, index),
                            directives: node.directives.clone(),
                            selection_set: transform_selection_set(&node.selection_set, arguments, index, false),
                        })
                    )
                },
                executable::Selection::FragmentSpread(node) => {
                    executable::Selection::FragmentSpread(
                        Node::new(executable::FragmentSpread {
                            fragment_name: node.fragment_name.clone(),
                            directives: node.directives.clone(),
                        })
                    )
                },
                executable::Selection::InlineFragment(node) => {
                    executable::Selection::InlineFragment(
                        Node::new(executable::InlineFragment {
                            type_condition: node.type_condition.clone(),
                            directives: node.directives.clone(),
                            selection_set: transform_selection_set(&node.selection_set, arguments, index, false),
                        })
                    )
                },
            }
        }).collect(),
    }
}

// // to delete
// fn query_batching_for_contextual_args(
//     operation: &str,
//     contextual_arguments: &Option<ContextualArguments>,
// ) -> Result<Option<String>, ContextBatchingError> {
//     if let Some(ContextualArguments { arguments, count }) = contextual_arguments {
//         let parser = apollo_compiler::Parser::new()
//             .parse_ast(operation, "")
//             .map_err(ContextBatchingError::InvalidDocument)?;
//         if let Some(mut operation) = parser
//             .definitions
//             .into_iter()
//             .find_map(|definition| definition.as_operation_definition().cloned())
//         {
//             let mut new_variables = vec![];
//             if operation
//                 .variables
//                 .iter()
//                 .any(|v| arguments.contains(v.name.as_str()))
//             {
//                 let new_selection_set: Vec<_> = (0..*count)
//                     .map(|i| {
//                         // TODO: Unwrap
//                         let mut s = operation
//                             .selection_set
//                             .first()
//                             .ok_or_else(|| ContextBatchingError::NoSelectionSet)?
//                             .clone();
//                         if let ast::Selection::Field(f) = &mut s {
//                             let f = f.make_mut();
//                             f.alias = Some(Name::new_unchecked(format!("_{}", i).into()));
//                         }

//                         for v in &operation.variables {
//                             if arguments.contains(v.name.as_str()) {
//                                 let mut cloned = v.clone();
//                                 let new_variable = cloned.make_mut();
//                                 new_variable.name =
//                                     Name::new_unchecked(format!("{}_{}", v.name, i).into());
//                                 new_variables.push(Node::new(new_variable.clone()));

//                                 s = rename_variables(s, v.name.clone(), new_variable.name.clone());
//                             } else if !new_variables.iter().any(|var| var.name == v.name) {
//                                 new_variables.push(v.clone());
//                             }
//                         }

//                         Ok(s)
//                     })
//                     .collect::<Result<Vec<_>, _>>()?;

//                 let new_operation = operation.make_mut();
//                 new_operation.selection_set = new_selection_set;
//                 new_operation.variables = new_variables;

//                 return Ok(Some(new_operation.serialize().no_indent().to_string()));
//             }
//         }
//     }

//     Ok(None)
// }

// fn rename_variables(selection_set: ast::Selection, from: Name, to: Name) -> ast::Selection {
//     match selection_set {
//         ast::Selection::Field(f) => {
//             let mut new = f.clone();

//             let as_mut = new.make_mut();

//             as_mut.arguments.iter_mut().for_each(|arg| {
//                 if arg.value.as_variable() == Some(&from) {
//                     arg.make_mut().value = ast::Value::Variable(to.clone()).into();
//                 }
//             });

//             as_mut.selection_set = as_mut
//                 .selection_set
//                 .clone()
//                 .into_iter()
//                 .map(|s| rename_variables(s, from.clone(), to.clone()))
//                 .collect();

//             ast::Selection::Field(new)
//         }
//         ast::Selection::InlineFragment(f) => {
//             let mut new = f.clone();
//             new.make_mut().selection_set = f
//                 .selection_set
//                 .clone()
//                 .into_iter()
//                 .map(|s| rename_variables(s, from.clone(), to.clone()))
//                 .collect();
//             ast::Selection::InlineFragment(new)
//         }
//         ast::Selection::FragmentSpread(f) => ast::Selection::FragmentSpread(f),
//     }
// }

#[derive(Debug)]
pub enum ContextBatchingError {
    InvalidDocument(WithErrors<Document>),
    NoSelectionSet,
}

// #[derive(Debug)]
// #[test]
// fn test_query_batching_for_contextual_args() {
//     let old_query = "query QueryLL__Subgraph1__1($representations:[_Any!]!$Subgraph1_U_field_a:String!){_entities(representations:$representations){...on U{id field(a:$Subgraph1_U_field_a)}}}";
//     let mut ctx_args = HashSet::new();
//     ctx_args.insert("Subgraph1_U_field_a".to_string());
//     let contextual_args = Some((ctx_args, 2));

//     let expected = "query QueryLL__Subgraph1__1($representations: [_Any!]!, $Subgraph1_U_field_a_0: String!, $Subgraph1_U_field_a_1: String!) { _0: _entities(representations: $representations) { ... on U { id field(a: $Subgraph1_U_field_a_0) } } _1: _entities(representations: $representations) { ... on U { id field(a: $Subgraph1_U_field_a_1) } } }";

//     assert_eq!(
//         expected,
//         query_batching_for_contextual_args(old_query, &contextual_args)
//             .unwrap()
//             .unwrap()
//     );
// }
