//! `@method` — convenience sugar over `@source(methods:)`.
//!
//! `@method` on an object/interface type registers a **nullary** custom `->`
//! method (Fernando Koch's proposal, realized atop the methods substrate). The
//! method is named after the type (or the `as:` argument), and its body is the
//! `selection:` argument, or — when omitted — auto-derived from the type's
//! field names. Invocation semantics are the ordinary method semantics:
//! `input->TypeName` applies the body to `input` (the `@ { … }` apply-to-input
//! shape from the design).
//!
//! Auto-derive over **object-typed** fields is the parked Fernando co-design
//! item: for v1, a field whose type is itself an object requires an explicit
//! `selection:` (validation reports an error otherwise — see
//! `validation::methods`). This module's extraction is lenient (it produces
//! whatever methods it can); the user-facing errors live in validation.

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::schema::ExtendedType;

use super::connect::CONNECT_SELECTION_ARGUMENT_NAME;
use super::type_and_directive_specifications::METHOD_AS_ARGUMENT_NAME;
use crate::connectors::CompiledMethod;
use crate::connectors::ConnectSpec;

/// The method name and body a `@method` directive desugars to.
pub(crate) struct DerivedMethod {
    pub(crate) name: String,
    pub(crate) body: String,
}

/// Collect the `(name, body)` pairs each `@method` directive desugars to,
/// across every object/interface type in the schema. Pure (no parsing); used
/// both to build [`CompiledMethod`]s for the registry and to drive validation.
pub(crate) fn derived_methods(schema: &Schema, method_directive_name: &Name) -> Vec<DerivedMethod> {
    let mut methods = Vec::new();
    for (type_name, ty) in &schema.types {
        let (directives, field_names): (_, Vec<&str>) = match ty {
            ExtendedType::Object(obj) => (
                &obj.directives,
                obj.fields.keys().map(|k| k.as_str()).collect(),
            ),
            ExtendedType::Interface(iface) => (
                &iface.directives,
                iface.fields.keys().map(|k| k.as_str()).collect(),
            ),
            _ => continue,
        };

        for directive in directives
            .iter()
            .filter(|d| d.name == *method_directive_name)
        {
            let name = directive
                .specified_argument_by_name(METHOD_AS_ARGUMENT_NAME.as_str())
                .and_then(|value| value.as_str())
                .unwrap_or(type_name.as_str())
                .to_string();
            let body = match directive
                .specified_argument_by_name(CONNECT_SELECTION_ARGUMENT_NAME.as_str())
                .and_then(|value| value.as_str())
            {
                Some(selection) => selection.to_string(),
                // Auto-derive: select the type's fields by name.
                None => field_names.join(" "),
            };
            methods.push(DerivedMethod { name, body });
        }
    }
    methods
}

/// Compile the `@method`-derived methods into [`CompiledMethod`]s, leniently:
/// entries whose body fails to parse (e.g. an empty auto-derived body) are
/// skipped — validation reports those separately.
pub(crate) fn compiled_derived_methods(
    schema: &Schema,
    method_directive_name: &Name,
    spec: ConnectSpec,
) -> Vec<CompiledMethod> {
    derived_methods(schema, method_directive_name)
        .into_iter()
        .filter_map(|method| {
            CompiledMethod::parse(method.name, &method.body, spec)
                .ok()
                // Records that this method describes one object of a GraphQL
                // type, so applying it to a list is reported rather than
                // silently distributing.
                .map(CompiledMethod::derived_from_type)
        })
        .collect()
}
