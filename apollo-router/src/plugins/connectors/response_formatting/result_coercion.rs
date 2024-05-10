use apollo_compiler::executable::Field;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::Type;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;

use super::engine::execute_selection_set;
use super::resolver::type_name;
use super::response::FormattingDiagnostic;
use super::response::LinkedPath;
use super::response::LinkedPathElement;
use super::response::ResponseDataPathElement;
use super::JsonValue;

/// <https://spec.graphql.org/October2021/#CompleteValue()>
pub(super) fn complete_value<'a, 'b>(
    schema: &'a Valid<Schema>,
    document: &'a Valid<ExecutableDocument>,
    diagnostics: &'b mut Vec<FormattingDiagnostic>,
    path: LinkedPath<'b>,
    ty: &'a Type,
    resolved: &JsonValue,
    fields: &'a [&'a Field],
) -> JsonValue {
    macro_rules! field_diagnostic {
        ($($arg: tt)+) => {
            {
                diagnostics.push(FormattingDiagnostic::for_path(
                    format!($($arg)+),
                    path,
                ));
                return resolved.clone();
            }
        };
    }

    match resolved {
        JsonValue::Object(resolved_obj) => {
            let ty_name = match ty {
                Type::Named(name) | Type::NonNullNamed(name) => name,
                Type::List(_) | Type::NonNullList(_) => {
                    // If the schema expects a list, but the response is an object,
                    // we'll wrap it in a list to make a valid response. This might
                    // be unexpected, but it the alternative is doing nothing and
                    // letting the router's response validation silently drop the data.
                    // We'll also emit a diagnostic to help users identify this mismatch.
                    diagnostics.push(FormattingDiagnostic::for_path(
                        format!("List type {ty} resolved to an object"),
                        path,
                    ));

                    let inner_path = LinkedPathElement {
                        element: ResponseDataPathElement::ListIndex(0),
                        next: path,
                    };

                    let inner_resolved = JsonValue::Array(vec![resolved.clone()]);

                    return complete_value(
                        schema,
                        document,
                        diagnostics,
                        Some(&inner_path),
                        ty,
                        &inner_resolved,
                        fields,
                    );
                }
            };

            let Some(ty_def) = schema.types.get(ty_name) else {
                field_diagnostic!("Undefined type {ty_name}");
            };

            let (object_type_name, object_type) = match ty_def {
                ExtendedType::Interface(_) | ExtendedType::Union(_) => {
                    if let Some(object_type_name) = type_name(resolved_obj, schema, ty_def) {
                        if let Some(def) = schema.get_object(&object_type_name) {
                            (object_type_name, def)
                        } else {
                            field_diagnostic!(
                                "Resolver returned an object of type {object_type_name} \
                     not defined in the schema"
                            )
                        }
                    } else {
                        field_diagnostic!("Could not determine typename for {ty_name}")
                    }
                }
                ExtendedType::Object(def) => (ty_name.to_string(), def),
                _ => field_diagnostic!("Type {ty_name} is not a composite type"),
            };

            JsonValue::Object(execute_selection_set(
                schema,
                document,
                diagnostics,
                path,
                &object_type_name,
                object_type,
                resolved_obj,
                fields
                    .iter()
                    .flat_map(|field| &field.selection_set.selections),
            ))
        }

        JsonValue::Array(iter) => match ty {
            Type::Named(_) | Type::NonNullNamed(_) => {
                field_diagnostic!("Non-list type {ty} resolved to a list")
            }
            Type::List(inner_ty) | Type::NonNullList(inner_ty) => {
                let mut completed_list = vec![];
                for (index, inner_resolved) in iter.iter().enumerate() {
                    let inner_path = LinkedPathElement {
                        element: ResponseDataPathElement::ListIndex(index),
                        next: path,
                    };

                    let inner_result = complete_value(
                        schema,
                        document,
                        diagnostics,
                        Some(&inner_path),
                        inner_ty,
                        inner_resolved,
                        fields,
                    );

                    completed_list.push(inner_result);
                }

                completed_list.into()
            }
        },

        _ => resolved.clone(),
    }
}
