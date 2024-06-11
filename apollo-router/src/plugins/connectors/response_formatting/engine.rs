use std::collections::HashSet;

use apollo_compiler::ast::Name;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::Selection;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::FieldDefinition;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;
use indexmap::IndexMap;

use super::resolver::resolve_field;
use super::resolver::ResolverInfo;
use super::response::FormattingDiagnostic;
use super::response::LinkedPath;
use super::response::LinkedPathElement;
use super::response::ResponseDataPathElement;
use super::result_coercion::complete_value;
use super::JsonMap;
use super::JsonValue;

const TYPENAME: &str = "__typename";

pub(crate) fn execute(
    schema: &Valid<Schema>,
    document: &Valid<ExecutableDocument>,
    diagnostics: &mut Vec<FormattingDiagnostic>,
    object_value: JsonMap,
) -> JsonMap {
    // documents from the query planner will always have a single operation
    let Ok(operation) = document.get_operation(None) else {
        return object_value;
    };

    let object_type_name = operation.operation_type.default_type_name();
    let object_type_name = object_type_name.as_str();

    let Some(object_type) = schema.get_object(object_type_name) else {
        return object_value;
    };

    let path = None;
    execute_selection_set(
        schema,
        document,
        diagnostics,
        path,
        object_type_name,
        object_type,
        &object_value,
        &operation.selection_set.selections,
    )
}

/// <https://spec.graphql.org/October2021/#ExecuteSelectionSet()>
#[allow(clippy::too_many_arguments)]
pub(super) fn execute_selection_set<'a>(
    schema: &Valid<Schema>,
    document: &'a Valid<ExecutableDocument>,
    diagnostics: &mut Vec<FormattingDiagnostic>,
    path: LinkedPath<'_>,
    object_type_name: &str,
    object_type: &ObjectType,
    object_value: &JsonMap,
    selections: impl IntoIterator<Item = &'a Selection>,
) -> JsonMap {
    let mut grouped_field_set = IndexMap::new();
    collect_fields(
        schema,
        document,
        object_type_name,
        object_type,
        selections,
        &mut HashSet::new(),
        &mut grouped_field_set,
    );

    let mut response_map = JsonMap::with_capacity(grouped_field_set.len());

    for (&response_key, fields) in &grouped_field_set {
        // Indexing should not panic: `collect_fields` only creates a `Vec` to push to it
        let field_name = &fields[0].name;
        let Ok(field_def) = schema.type_field(object_type_name, field_name) else {
            // emitting a diagnostic here would be redundant with Selection::apply_to
            continue;
        };
        let value = if field_name == TYPENAME {
            JsonValue::from(object_type_name)
        } else {
            let field_path = LinkedPathElement {
                element: ResponseDataPathElement::Field(response_key.clone()),
                next: path,
            };
            execute_field(
                schema,
                document,
                diagnostics,
                Some(&field_path),
                object_value,
                field_def,
                fields,
            )
        };
        response_map.insert(response_key.as_str(), value);
    }

    response_map
}

/// <https://spec.graphql.org/October2021/#CollectFields()>
fn collect_fields<'a>(
    schema: &Schema,
    document: &'a ExecutableDocument,
    object_type_name: &str,
    object_type: &ObjectType,
    selections: impl IntoIterator<Item = &'a Selection>,
    visited_fragments: &mut HashSet<&'a Name>,
    grouped_fields: &mut IndexMap<&'a Name, Vec<&'a Field>>,
) {
    for selection in selections {
        match selection {
            Selection::Field(field) => grouped_fields
                .entry(field.response_key())
                .or_default()
                .push(field.as_ref()),
            Selection::FragmentSpread(spread) => {
                let new = visited_fragments.insert(&spread.fragment_name);
                if !new {
                    continue;
                }
                let Some(fragment) = document.fragments.get(&spread.fragment_name) else {
                    continue;
                };
                if !does_fragment_type_apply(
                    schema,
                    object_type_name,
                    object_type,
                    fragment.type_condition(),
                ) {
                    continue;
                }
                collect_fields(
                    schema,
                    document,
                    object_type_name,
                    object_type,
                    &fragment.selection_set.selections,
                    visited_fragments,
                    grouped_fields,
                )
            }
            Selection::InlineFragment(inline) => {
                if let Some(condition) = &inline.type_condition {
                    if !does_fragment_type_apply(schema, object_type_name, object_type, condition) {
                        continue;
                    }
                }
                collect_fields(
                    schema,
                    document,
                    object_type_name,
                    object_type,
                    &inline.selection_set.selections,
                    visited_fragments,
                    grouped_fields,
                )
            }
        }
    }
}

/// <https://spec.graphql.org/October2021/#DoesFragmentTypeApply()>
fn does_fragment_type_apply(
    schema: &Schema,
    object_type_name: &str,
    object_type: &ObjectType,
    fragment_type: &Name,
) -> bool {
    match schema.types.get(fragment_type) {
        Some(ExtendedType::Object(_)) => fragment_type == object_type_name,
        Some(ExtendedType::Interface(_)) => {
            object_type.implements_interfaces.contains(fragment_type)
        }
        Some(ExtendedType::Union(def)) => def.members.contains(object_type_name),
        // Undefined or not an output type: validation should have caught this
        _ => false,
    }
}

/// <https://spec.graphql.org/October2021/#ExecuteField()>
fn execute_field(
    schema: &Valid<Schema>,
    document: &Valid<ExecutableDocument>,
    diagnostics: &mut Vec<FormattingDiagnostic>,
    path: LinkedPath<'_>,
    object_value: &JsonMap,
    _field_def: &FieldDefinition,
    fields: &[&Field],
) -> JsonValue {
    let field = fields[0];

    let info = ResolverInfo {
        field_name: &field.name,
        response_key: field.response_key(),
    };

    let resolved_result = resolve_field(object_value, info);

    let completed_result = match resolved_result {
        Some(resolved) => complete_value(
            schema,
            document,
            diagnostics,
            path,
            field.ty(),
            &resolved,
            fields,
        ),
        None => {
            // emitting a diagnostic here would be redundant with Selection::apply_to
            JsonValue::Null
        }
    };

    completed_result
}

#[cfg(test)]
mod tests {
    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;

    #[test]
    fn test_aliases() {
        let schema = Schema::parse_and_validate(
            "type Query {hello: Hello} type Hello{id:ID a:ID}",
            "schema.graphql",
        )
        .unwrap();

        let document =
            ExecutableDocument::parse_and_validate(&schema, "{hello{id a:id b:a}}", "op.graphql")
                .unwrap();

        let data = serde_json_bytes::json!({
            "hello": {
                "id": "123",
                "a": "a"
            }
        });

        let mut diagnostics = vec![];
        let result = super::execute(
            &schema,
            &document,
            diagnostics.as_mut(),
            data.as_object().unwrap().clone(),
        );

        assert_eq!(
            result,
            *serde_json_bytes::json!({
                "hello": {
                    "id": "123",
                    "a": "123",
                    "b": "a"
                }
            })
            .as_object()
            .unwrap()
        );
    }

    #[test]
    fn test_list_coercion() {
        let schema = Schema::parse_and_validate(
            "type Query {hello: [Hello]} interface Hello{id:ID} type Foo implements Hello{id:ID a:ID}",
            "schema.graphql",
        )
        .unwrap();

        let document = ExecutableDocument::parse_and_validate(
            &schema,
            "{hello{__typename id ...on Foo {a}}}",
            "op.graphql",
        )
        .unwrap();

        let data = serde_json_bytes::json!({
            "hello": {
                "id": "123",
                "a": "a"
            }
        });

        let mut diagnostics = vec![];
        let result = super::execute(
            &schema,
            &document,
            diagnostics.as_mut(),
            data.as_object().unwrap().clone(),
        );

        assert_eq!(
            result,
            *serde_json_bytes::json!({
                "hello": [{
                    "__typename": "Foo",
                    "id": "123",
                    "a": "a"
                }]
            })
            .as_object()
            .unwrap()
        );
    }
}
