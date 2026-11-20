//! Lookup fields and the _stable keys_ they recall entities by.
//!
//! Composition derives `@key` resolvability from these (see [`super::normalize`]). A lookup reads
//! `@lookup` on the field and `@is` on its arguments (implicitly `@is(field: "<argument name>")`).

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::schema::ExtendedType;

use super::validation::lookup_reachable_types;
use crate::schema::field_selection_map;
use crate::schema::field_selection_map::SelectedValue;
use crate::schema::field_selection_map::validate::possible_types;
use crate::schema::field_selection_map::value::selections_for_type;

/// A `@lookup` field.
#[derive(Debug, Clone)]
pub(crate) struct Lookup {
    /// The named return type (an object, interface or union type).
    return_type: Name,
    /// Per argument, the `@is` map, or the implicit single-field map when it has no `@is`.
    argument_maps: Vec<SelectedValue>,
}

/// One way a lookup recalls one concrete entity type.
#[derive(Debug, Clone)]
pub(crate) struct LookupKey {
    pub(crate) concrete_type: Name,
    /// The key as field-set text.
    pub(crate) key: String,
}

impl Lookup {
    /// The keys by which this lookup recalls each of its possible concrete types. An argument
    /// whose map has several applicable alternatives (a `@oneOf` input) yields one key per
    /// alternative; several such arguments yield their combinations.
    pub(crate) fn keys(&self, schema: &Schema) -> Vec<LookupKey> {
        let mut keys = Vec::new();
        for concrete in possible_types(schema, &self.return_type) {
            let mut combinations = vec![String::new()];
            for map in &self.argument_maps {
                let options = selections_for_type(map, schema, &concrete);
                combinations = combinations
                    .iter()
                    .flat_map(|key| {
                        options
                            .iter()
                            .map(move |option| format!("{key} {option}").trim().to_string())
                    })
                    .collect();
            }
            keys.extend(
                combinations
                    .into_iter()
                    .filter(|key| !key.is_empty())
                    .map(|key| LookupKey {
                        concrete_type: concrete.clone(),
                        key,
                    }),
            );
        }
        keys
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
            ExtendedType::Object(object) => object.fields.values().collect(),
            ExtendedType::Interface(interface) => interface.fields.values().collect(),
            _ => continue,
        };
        'fields: for field in fields {
            if !field.directives.has(lookup_name) {
                continue;
            }
            let mut argument_maps = Vec::new();
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
                argument_maps.push(map);
            }
            lookups.push(Lookup {
                return_type: field.ty.inner_named_type().clone(),
                argument_maps,
            });
        }
    }
    lookups
}
