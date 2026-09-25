//! Lookup fields and the _stable keys_ they recall entities by.
//!
//! Shared by composition (which derives `@key` resolvability from lookups) and query planning
//! (which realizes entity jumps into a source schema as lookup calls). Both read the same
//! directives: `@lookup` on the field, and `@is` on its arguments (implicitly
//! `@is(field: "<argument name>")`).

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::schema::ExtendedType;

use super::validation::lookup_reachable_types;
use crate::schema::field_selection_map;
use crate::schema::field_selection_map::SelectedValue;
use crate::schema::field_selection_map::validate::possible_types;
use crate::schema::field_selection_map::value::SelectionTree;
use crate::schema::field_selection_map::value::selections_for_type;

/// A lookup argument and the selection map relating it to the entity.
#[derive(Debug, Clone)]
pub(crate) struct LookupArgument {
    pub(crate) name: Name,
    pub(crate) ty: Node<ast::Type>,
    /// The `@is` map, or the implicit single-field map when the argument has no `@is`.
    pub(crate) map: SelectedValue,
}

/// A `@lookup` field.
#[derive(Debug, Clone)]
pub(crate) struct Lookup {
    /// The type declaring the lookup field (the query root, or a type reached from it through
    /// argumentless fields).
    pub(crate) parent_type: Name,
    pub(crate) field: Name,
    /// The named return type (an object, interface or union type).
    pub(crate) return_type: Name,
    pub(crate) arguments: Vec<LookupArgument>,
}

/// One way a lookup recalls one concrete entity type: the key it takes, and which top-level
/// alternative of each argument's map produces the argument value.
#[derive(Debug, Clone)]
pub(crate) struct LookupKey {
    pub(crate) concrete_type: Name,
    pub(crate) key: SelectionTree,
    /// Per argument (in declaration order), the index of the map alternative used.
    pub(crate) alternatives: Vec<usize>,
}

impl Lookup {
    /// The keys by which this lookup recalls each of its possible concrete types. An argument
    /// whose map has several applicable alternatives (a `@oneOf` input) yields one key per
    /// alternative; several such arguments yield their combinations.
    pub(crate) fn keys(&self, schema: &Schema) -> Vec<LookupKey> {
        let mut keys = Vec::new();
        for concrete in possible_types(schema, &self.return_type) {
            let mut combinations: Vec<(SelectionTree, Vec<usize>)> =
                vec![(SelectionTree::default(), Vec::new())];
            for argument in &self.arguments {
                let options = selections_for_type(&argument.map, schema, &concrete);
                let mut next = Vec::new();
                for (tree, alternatives) in &combinations {
                    for (index, option) in &options {
                        let mut merged = tree.clone();
                        merged.merge(option);
                        let mut alternatives = alternatives.clone();
                        alternatives.push(*index);
                        next.push((merged, alternatives));
                    }
                }
                combinations = next;
            }
            for (key, alternatives) in combinations {
                if !key.is_empty() {
                    keys.push(LookupKey {
                        concrete_type: concrete.clone(),
                        key: key.canonical(),
                        alternatives,
                    });
                }
            }
        }
        keys
    }

    /// The chain of field names from the query root to this lookup field, inclusive: `[field]`
    /// for a lookup on the query root, `[lookups, productById]` for a nested one. `None` if the
    /// lookup's parent type is not reachable through argumentless, non-list fields.
    pub(crate) fn path_from_root(&self, schema: &Schema) -> Option<Vec<Name>> {
        let query = schema.schema_definition.query.as_ref()?.name.clone();
        // Breadth-first so the shortest chain wins.
        let mut parents: IndexMap<Name, Option<(Name, Name)>> = IndexMap::default();
        parents.insert(query.clone(), None);
        let mut queue = std::collections::VecDeque::from([query]);
        while let Some(type_name) = queue.pop_front() {
            if type_name == self.parent_type {
                let mut path = vec![self.field.clone()];
                let mut current = type_name;
                while let Some(Some((parent, field))) = parents.get(&current) {
                    path.push(field.clone());
                    current = parent.clone();
                }
                path.reverse();
                return Some(path);
            }
            let fields: Vec<&Node<FieldDefinition>> = match schema.types.get(&type_name) {
                Some(ExtendedType::Object(object)) => {
                    object.fields.values().map(|f| &f.node).collect()
                }
                Some(ExtendedType::Interface(interface)) => {
                    interface.fields.values().map(|f| &f.node).collect()
                }
                _ => continue,
            };
            for field in fields {
                if !field.arguments.is_empty() || field.ty.is_list() {
                    continue;
                }
                let target = field.ty.inner_named_type();
                if parents.contains_key(target)
                    || !matches!(
                        schema.types.get(target),
                        Some(ExtendedType::Object(_) | ExtendedType::Interface(_))
                    )
                {
                    continue;
                }
                parents.insert(
                    target.clone(),
                    Some((type_name.clone(), field.name.clone())),
                );
                queue.push_back(target.clone());
            }
        }
        None
    }
}

/// Every lookup field of `schema` that is reachable from the query root, given the in-schema names
/// of `@lookup` and `@is`. Lookups whose `@is` does not parse are skipped (validation reports
/// them).
pub(crate) fn collect_lookups(schema: &Schema, lookup_name: &Name, is_name: &Name) -> Vec<Lookup> {
    let reachable = lookup_reachable_types(schema);
    let mut lookups = Vec::new();
    for (type_name, ty) in &schema.types {
        if !reachable.contains(type_name) {
            continue;
        }
        let fields: Vec<&Node<FieldDefinition>> = match ty {
            ExtendedType::Object(object) => object.fields.values().map(|f| &f.node).collect(),
            ExtendedType::Interface(interface) => {
                interface.fields.values().map(|f| &f.node).collect()
            }
            _ => continue,
        };
        'fields: for field in fields {
            if !field.directives.has(lookup_name) {
                continue;
            }
            let mut arguments = Vec::new();
            for argument in &field.arguments {
                let map = match argument.directives.get(is_name) {
                    Some(is) => {
                        let Some(map) = is
                            .specified_argument_by_name("field")
                            .and_then(|v| v.as_str())
                            .and_then(|text| field_selection_map::parse(text).ok())
                        else {
                            continue 'fields;
                        };
                        map
                    }
                    None => SelectedValue::field(argument.name.clone()),
                };
                arguments.push(LookupArgument {
                    name: argument.name.clone(),
                    ty: argument.ty.clone(),
                    map,
                });
            }
            lookups.push(Lookup {
                parent_type: type_name.clone(),
                field: field.name.clone(),
                return_type: field.ty.inner_named_type().clone(),
                arguments,
            });
        }
    }
    lookups
}
