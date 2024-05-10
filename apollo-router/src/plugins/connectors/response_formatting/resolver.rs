use std::collections::HashSet;

use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::Name;
use apollo_compiler::validation::Valid;
use apollo_compiler::Schema;
use itertools::Itertools;
use serde_json_bytes::Value;

use super::JsonMap;
use super::JsonValue;

pub(crate) struct ResolverInfo<'a> {
    pub(crate) field_name: &'a str,
    pub(crate) response_key: &'a str,
}

pub(super) fn resolve_field(object_value: &JsonMap, info: ResolverInfo) -> Option<JsonValue> {
    let ResolverInfo {
        field_name,
        response_key,
        ..
    } = info;

    if let Some(value) = object_value.get(field_name) {
        Some(value.clone())
    } else {
        object_value.get(response_key).cloned()
    }
}

pub(super) fn type_name<'a>(
    object: &'a JsonMap,
    schema: &'a Valid<Schema>,
    ty: &'a ExtendedType,
) -> Option<String> {
    if let Some(Value::String(typename)) = object.get("__typename") {
        if schema.get_object(typename.as_str()).is_some() {
            return Some(typename.as_str().to_string());
        }
    }

    match_possible_type(schema, ty, object)
}

/// This function determines a type name given an abstract type and an object.
/// The current heuristic is to compare field names on the possible types and
/// field names on the object. The tie breaker is which ever possible type name
/// comes first alphabetically.
fn match_possible_type<'a>(
    schema: &'a Valid<Schema>,
    ty: &'a ExtendedType,
    object: &'a JsonMap,
) -> Option<String> {
    let impls = schema.implementers_map();
    let field_names = object
        .keys()
        .filter_map(|n| Name::new(n.as_str()).ok())
        .collect::<HashSet<_>>();

    match ty {
        ExtendedType::Interface(i) => {
            if let Some(possible_types) = impls.get(&i.name) {
                let match_scores =
                    rank_possible_choices(schema, possible_types.iter(), &field_names);
                return match_scores.first().map(|(_, name)| name.to_string());
            }
        }

        ExtendedType::Union(u) => {
            let possible_types = u
                .members
                .iter()
                .cloned()
                .map(|c| c.name)
                .collect::<HashSet<Name>>();
            let match_scores = rank_possible_choices(schema, possible_types.iter(), &field_names);
            return match_scores.first().map(|(_, name)| name.to_string());
        }

        _ => {} // do nothing and hope this is handled elsewhere
    }
    None
}

fn rank_possible_choices<'a>(
    schema: &'a Schema,
    possible_types: impl Iterator<Item = &'a Name>,
    field_names: &'a HashSet<Name>,
) -> Vec<(i32, &'a Name)> {
    possible_types
        .map(|name| {
            schema
                .get_object(name)
                .map(|possible_type| {
                    let possible_field_names = possible_type
                        .fields
                        .keys()
                        .cloned()
                        .collect::<HashSet<Name>>();
                    let count = field_names.intersection(&possible_field_names).count() as i32;

                    // so we can sort ascending for both the score and name, we'll use negative numbers:
                    // more matched field names -> lower score
                    (-count, name)
                })
                .unwrap_or((0, name))
        })
        .sorted()
        .collect::<Vec<_>>()
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use serde_json_bytes::json;

    #[test]
    fn test_match_possible_type() {
        let schema = Schema::parse_and_validate(
            "
        type Query {
            hello: String
        }

        interface If {
            one: ID
        }

        type A implements If {
            one: ID
            two: ID
        }

        type B implements If {
            one: ID
            three: ID
        }

        # same as B, will never match
        type C implements If {
            one: ID
            three: ID
        }

        type D implements If {
            one: ID
            three: ID
            four: ID
        }
        ",
            "schema.graphql",
        )
        .unwrap();

        let tests = vec![
            (json!({ "one": "1" }), Some("A".to_string())),
            (json!({ "one": "1", "two": "2" }), Some("A".to_string())),
            (json!({ "one": "1", "three": "3" }), Some("B".to_string())),
            (
                json!({ "one": "1", "three": "3", "four": "4" }),
                Some("D".to_string()),
            ),
            (json!({ "one": "1", "unknown": "x" }), Some("A".to_string())),
        ];

        for (i, (object, expected)) in tests.iter().enumerate() {
            let object = object.as_object().unwrap();
            let actual =
                super::match_possible_type(&schema, schema.types.get("If").unwrap(), object);

            assert_eq!(
                expected, &actual,
                "{}: __typename for {:?} should be {:?} but was {:?}",
                i, object, expected, actual
            );
        }
    }
}
