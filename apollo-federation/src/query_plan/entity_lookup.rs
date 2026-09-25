//! Entity fetches through GraphQL Federation `@lookup` fields.
//!
//! An `_entities` fetch sends all entities as one `representations` variable. A lookup fetch
//! instead runs one operation per entity: the operation calls the lookup field with one variable
//! per argument, and the executor computes those variables from each entity's representation
//! (the same data an `_entities` fetch would send). The planner emits one operation plus the
//! templates below; how the per-entity variable sets are sent (one request each, or batched per
//! graphql-over-http) is decided by the executor.

use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::Value;

/// How a lookup fetch resolves each entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityLookup {
    /// Response keys from the operation root to the lookup field: `["productById"]` for a lookup on
    /// the query root, `["lookups", "productById"]` for one reached through argumentless fields.
    pub path: Vec<String>,
    /// Variables computed per entity, from its representation.
    pub variables: Vec<LookupVariable>,
}

/// An operation variable whose value is computed per entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LookupVariable {
    pub name: String,
    /// Whether the variable's type is non-null. An entity for which a non-null variable evaluates
    /// to null cannot be looked up, and resolves to null.
    pub non_null: bool,
    pub value: ValueTemplate,
}

/// A value computed from an entity representation (a JSON object). Built from `@is`/`@require`
/// field selection maps; type conditions are resolved against the representation's
/// `__typename`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ValueTemplate {
    /// The value at a path of response keys. Missing or null values evaluate to null.
    Path(Vec<String>),
    /// An input object whose fields are computed by templates.
    Object(Vec<(String, ValueTemplate)>),
    /// The list at `path`, with `item` evaluated relative to each element.
    List {
        path: Vec<String>,
        item: Box<ValueTemplate>,
    },
    /// The first alternative whose type condition admits the current object's `__typename` (or
    /// has none) and which evaluates to a non-null value.
    Alternatives(Vec<TemplateAlternative>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateAlternative {
    /// Concrete type names the alternative applies to; `None` applies to any object.
    pub types: Option<Vec<String>>,
    pub value: ValueTemplate,
}

impl std::fmt::Display for ValueTemplate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValueTemplate::Path(path) => f.write_str(&path.join(".")),
            ValueTemplate::Object(fields) => {
                f.write_str("{ ")?;
                for (i, (name, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{name}: {value}")?;
                }
                f.write_str(" }")
            }
            ValueTemplate::List { path, item } => write!(f, "{}[{item}]", path.join(".")),
            ValueTemplate::Alternatives(alternatives) => {
                for (i, alternative) in alternatives.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" | ")?;
                    }
                    if let Some(types) = &alternative.types {
                        write!(f, "<{}> ", types.join(" | "))?;
                    }
                    write!(f, "{}", alternative.value)?;
                }
                Ok(())
            }
        }
    }
}

fn at_path<'v>(value: &'v Value, path: &[String]) -> Option<&'v Value> {
    let mut current = value;
    for key in path {
        current = current.as_object()?.get(key.as_str())?;
    }
    Some(current)
}

impl ValueTemplate {
    /// Evaluate against `value` (a representation, or a nested object of one). Returns
    /// `Value::Null` when the data needed is missing or null.
    pub fn evaluate(&self, value: &Value) -> Value {
        match self {
            ValueTemplate::Path(path) => at_path(value, path).cloned().unwrap_or(Value::Null),
            ValueTemplate::Object(fields) => {
                let mut object = serde_json_bytes::Map::new();
                for (name, template) in fields {
                    object.insert(name.as_str(), template.evaluate(value));
                }
                Value::Object(object)
            }
            ValueTemplate::List { path, item } => match at_path(value, path) {
                Some(Value::Array(elements)) => {
                    Value::Array(elements.iter().map(|e| item.evaluate(e)).collect())
                }
                _ => Value::Null,
            },
            ValueTemplate::Alternatives(alternatives) => {
                let typename = value
                    .as_object()
                    .and_then(|o| o.get("__typename"))
                    .and_then(|t| t.as_str());
                for alternative in alternatives {
                    if let Some(types) = &alternative.types {
                        match typename {
                            Some(typename) if types.iter().any(|t| t == typename) => {}
                            _ => continue,
                        }
                    }
                    let evaluated = alternative.value.evaluate(value);
                    if !evaluated.is_null() && !contains_null_leaf(&evaluated) {
                        return evaluated;
                    }
                }
                Value::Null
            }
        }
    }
}

/// Whether a computed input object has a null field (an alternative that did not fully apply).
fn contains_null_leaf(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Object(object) => object.values().any(contains_null_leaf),
        _ => false,
    }
}

impl EntityLookup {
    /// The variables for one entity, or `None` if a non-null variable evaluates to null (the
    /// entity cannot be looked up).
    pub fn variables_for(
        &self,
        representation: &Value,
    ) -> Option<serde_json_bytes::Map<serde_json_bytes::ByteString, Value>> {
        let mut variables = serde_json_bytes::Map::new();
        for variable in &self.variables {
            let value = variable.value.evaluate(representation);
            if variable.non_null && value.is_null() {
                return None;
            }
            variables.insert(variable.name.as_str(), value);
        }
        Some(variables)
    }

    /// The lookup field's value in one lookup response's `data`.
    pub fn result_in<'v>(&self, data: &'v Value) -> Option<&'v Value> {
        at_path(data, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;

    fn path(p: &[&str]) -> ValueTemplate {
        ValueTemplate::Path(p.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn evaluates_paths_objects_and_lists() {
        let representation = json!({
            "__typename": "Product",
            "id": "1",
            "dimension": { "size": 2, "weight": 3 },
            "parts": [{ "id": "a" }, { "id": "b" }],
        });
        assert_eq!(path(&["id"]).evaluate(&representation), json!("1"));
        assert_eq!(
            path(&["dimension", "weight"]).evaluate(&representation),
            json!(3)
        );
        assert_eq!(path(&["missing"]).evaluate(&representation), Value::Null);
        let object = ValueTemplate::Object(vec![
            ("s".to_string(), path(&["dimension", "size"])),
            ("w".to_string(), path(&["dimension", "weight"])),
        ]);
        assert_eq!(object.evaluate(&representation), json!({ "s": 2, "w": 3 }));
        let list = ValueTemplate::List {
            path: vec!["parts".to_string()],
            item: Box::new(path(&["id"])),
        };
        assert_eq!(list.evaluate(&representation), json!(["a", "b"]));
    }

    #[test]
    fn selects_alternatives_by_type_and_availability() {
        let alternatives = ValueTemplate::Alternatives(vec![
            TemplateAlternative {
                types: Some(vec!["Book".to_string()]),
                value: ValueTemplate::Object(vec![("isbn".to_string(), path(&["isbn"]))]),
            },
            TemplateAlternative {
                types: None,
                value: ValueTemplate::Object(vec![("upc".to_string(), path(&["upc"]))]),
            },
        ]);
        assert_eq!(
            alternatives.evaluate(&json!({ "__typename": "Book", "isbn": "x" })),
            json!({ "isbn": "x" })
        );
        assert_eq!(
            alternatives.evaluate(&json!({ "__typename": "Movie", "upc": "y" })),
            json!({ "upc": "y" })
        );
        // An alternative with a missing field is skipped.
        assert_eq!(
            alternatives.evaluate(&json!({ "__typename": "Book", "upc": "y" })),
            json!({ "upc": "y" })
        );
        assert_eq!(
            alternatives.evaluate(&json!({ "__typename": "Movie" })),
            Value::Null
        );
    }

    #[test]
    fn skips_entities_missing_non_null_variables() {
        let lookup = EntityLookup {
            path: vec!["productById".to_string()],
            variables: vec![LookupVariable {
                name: "lookupArgument_0".to_string(),
                non_null: true,
                value: path(&["id"]),
            }],
        };
        assert!(lookup.variables_for(&json!({ "id": "1" })).is_some());
        assert!(lookup.variables_for(&json!({ "id": null })).is_none());
        assert_eq!(
            lookup.result_in(&json!({ "productById": { "name": "x" } })),
            Some(&json!({ "name": "x" }))
        );
    }
}
