use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::executable::FragmentMap;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use rand::Rng;
use serde_json_bytes::Map;
use serde_json_bytes::Value;
use serde_json_bytes::json;

use crate::spec::TYPENAME;

pub(crate) fn random_value_for_operation<R: Rng>(
    rng: &mut R,
    doc: &Valid<ExecutableDocument>,
    operation_name: Option<String>,
    schema: &Valid<Schema>,
) -> Value {
    let Ok(operation) = doc.operations.get(operation_name.as_deref()) else {
        return json!({ "data": null });
    };

    json!({
        "data": random_value_for_selection_set(
            rng,
            &operation.selection_set,
            &doc.fragments,
            schema,
        )
    })
}

fn random_value_for_selection_set<R: Rng>(
    rng: &mut R,
    selection_set: &SelectionSet,
    fragments: &FragmentMap,
    schema: &Valid<Schema>,
) -> Value {
    let mut result = Map::new();

    for selection in &selection_set.selections {
        match selection {
            Selection::Field(field) => {
                if field.name == TYPENAME {
                    result.insert(
                        field.name.to_string(),
                        Value::String(selection_set.ty.to_string().into()),
                    );
                } else if field.selection_set.is_empty() && !field.ty().is_list() {
                    result.insert(
                        field.name.to_string(),
                        random_value_for_leaf_field(rng, field.ty().inner_named_type(), schema),
                    );
                } else if field.selection_set.is_empty() && field.ty().is_list() {
                    result.insert(
                        field.name.to_string(),
                        repeated(rng, |r| {
                            random_value_for_leaf_field(r, field.ty().inner_named_type(), schema)
                        }),
                    );
                } else if !field.selection_set.is_empty() && !field.ty().is_list() {
                    result.insert(
                        field.name.to_string(),
                        random_value_for_selection_set(
                            rng,
                            &field.selection_set,
                            fragments,
                            schema,
                        ),
                    );
                } else {
                    result.insert(
                        field.name.to_string(),
                        repeated(rng, |r| {
                            random_value_for_selection_set(
                                r,
                                &field.selection_set,
                                fragments,
                                schema,
                            )
                        }),
                    );
                }
            }
            Selection::FragmentSpread(fragment) => {
                if let Some(fragment_def) = fragments.get(&fragment.fragment_name) {
                    let value = random_value_for_selection_set(
                        rng,
                        &fragment_def.selection_set,
                        fragments,
                        schema,
                    );
                    if let Some(value_obj) = value.as_object() {
                        result.extend(value_obj.clone());
                    }
                }
            }
            Selection::InlineFragment(inline_fragment) => {
                let value = random_value_for_selection_set(
                    rng,
                    &inline_fragment.selection_set,
                    fragments,
                    schema,
                );
                if let Some(value_obj) = value.as_object() {
                    result.extend(value_obj.clone());
                }
            }
        }
    }

    Value::Object(result)
}

fn repeated<R: Rng, G: Fn(&mut R) -> Value>(rng: &mut R, generator: G) -> Value {
    let num_values = rng.random_range(0..=5);
    let mut values = Vec::with_capacity(num_values);
    for _ in 0..num_values {
        values.push(generator(rng));
    }
    Value::Array(values)
}

fn random_value_for_leaf_field<R: Rng>(
    rng: &mut R,
    type_name: &Name,
    schema: &Valid<Schema>,
) -> Value {
    let extended_ty = schema.types.get(type_name).unwrap();
    match extended_ty {
        ExtendedType::Enum(enum_ty) => {
            let enum_idx = rng.random_range(0..enum_ty.values.len());
            let enum_value = enum_ty.values.values().nth(enum_idx).unwrap();
            Value::String(enum_value.value.to_string().into())
        }
        ExtendedType::Scalar(scalar) => {
            if scalar.name == "Boolean" {
                let random_bool = rng.random_bool(0.5);
                Value::Bool(random_bool)
            } else if scalar.name == "Int" || scalar.name == "ID" {
                let random_int = rng.random_range(0..=100);
                Value::Number(random_int.into())
            } else if scalar.name == "Float" {
                let random_float = rng.random_range(0.0..100.0);
                Value::Number(serde_json::Number::from_f64(random_float).unwrap())
            } else if scalar.name == "String" {
                let random_string: String = (0..10)
                    .map(|_| rng.sample(rand::distr::Alphanumeric) as char)
                    .collect();
                Value::String(random_string.into())
            } else {
                // Likely a custom scalar
                panic!("Cannot generate random value for type: {type_name}")
            }
        }

        _ => unreachable!(
            "We are in a field with an empty selection set, so it must be a scalar or enum type"
        ),
    }
}
