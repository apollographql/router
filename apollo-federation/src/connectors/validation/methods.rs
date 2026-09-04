//! Validates `@source(methods:)` and `@connect(methods:)` — reusable, type-inferred
//! custom `->` method definitions (connect/v0.5+).
//!
//! Three layers of checking happen here, all at compose time:
//! 1. **Version gate** — `methods` only exists in connect v0.5 or later.
//! 2. **Per-body parse** — each method body (header + selection) must parse.
//! 3. **Global registry invariants** — across *every* `methods:` block on either
//!    directive, plus `@method`-derived methods: duplicate names, builtin
//!    shadowing, and inlinability (no cycles).
//!
//! Layer 3 spanning both directives is what makes `@connect(methods:)` colocation
//! rather than scoping: two connectors cannot each declare their own `foo`,
//! because there is only one namespace. See [`crate::connectors::spec::methods`].

use std::ops::Range;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::Value;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::ExtendedType;

use super::Code;
use super::Message;
use super::Severity;
use super::graphql::SchemaInfo;
use crate::connectors::CompiledMethod;
use crate::connectors::ConnectSpec;
use crate::connectors::JSONSelection;
use crate::connectors::MethodError;
use crate::connectors::MethodRegistry;
use crate::connectors::json_selection::methods::is_builtin_method_name;
use crate::connectors::json_selection::methods::is_reserved_method_name;
use crate::connectors::spec::connect::CONNECT_SELECTION_ARGUMENT_NAME;
use crate::connectors::spec::connect::IS_SUCCESS_ARGUMENT_NAME;
use crate::connectors::spec::methods::METHODS_ARGUMENT_NAME;

/// Every `methods:` argument value in the schema, paired with the name of the
/// directive that declared it so messages can name the right coordinate.
///
/// Two declaration sites are collected: `@source` on the schema definition, and
/// `@connect` wherever it may be applied (object/interface/union types, and
/// object/interface fields). Both land in one flat list precisely because they
/// share one namespace — no caller should care which directive a method came from
/// except when phrasing an error.
fn methods_arguments<'schema>(
    schema: &'schema Schema,
    source_directive_name: &'schema Name,
    connect_directive_name: &'schema Name,
) -> Vec<(&'schema Name, &'schema Node<Value>)> {
    let methods_argument = |directive: &'schema Directive| {
        directive.specified_argument_by_name(METHODS_ARGUMENT_NAME.as_str())
    };

    let mut found: Vec<(&Name, &Node<Value>)> = schema
        .schema_definition
        .directives
        .iter()
        .filter(|directive| directive.name == *source_directive_name)
        .filter_map(|directive| methods_argument(directive))
        .map(|value| (source_directive_name, value))
        .collect();

    found.extend(
        applied_directives(schema)
            .into_iter()
            .filter(|directive| directive.name == *connect_directive_name)
            .filter_map(methods_argument)
            .map(|value| (connect_directive_name, value)),
    );

    found
}

/// Every directive applied to a type or to one of its fields — the places
/// `@connect` and `@method` can appear. (`@source` lives on the schema
/// definition instead and is read directly.)
fn applied_directives(schema: &Schema) -> Vec<&Directive> {
    let mut found: Vec<&Directive> = Vec::new();
    for ty in schema.types.values() {
        let (type_directives, fields) = match ty {
            ExtendedType::Object(obj) => (&obj.directives, Some(&obj.fields)),
            ExtendedType::Interface(iface) => (&iface.directives, Some(&iface.fields)),
            ExtendedType::Union(union) => (&union.directives, None),
            _ => continue,
        };
        // Type-level directives are `Component<Directive>` (one more layer of
        // indirection than the `Node<Directive>` on fields).
        found.extend(type_directives.iter().map(|directive| &***directive));
        for field in fields.into_iter().flat_map(|fields| fields.values()) {
            found.extend(field.directives.iter().map(|directive| &**directive));
        }
    }
    found
}

/// Build the global method registry from every `methods:` block, leniently:
/// method bodies that fail to parse are skipped (their errors are reported by
/// [`validate`]), and if the assembled set is not admissible (duplicate,
/// builtin shadow, or cycle) the whole registry is dropped (`None`). The result
/// is stored on [`SchemaInfo`] so the shape pass can resolve `->customMethod`
/// calls during composition; this never itself emits user-facing errors.
pub(super) fn build_registry(
    schema: &Schema,
    source_directive_name: &Name,
    connect_directive_name: &Name,
    method_directive_name: &Name,
    spec: ConnectSpec,
) -> Option<Arc<MethodRegistry>> {
    let mut compiled = Vec::new();

    // `@source(methods:)` and `@connect(methods:)` entries, in one namespace.
    for (_directive_name, methods_value) in
        methods_arguments(schema, source_directive_name, connect_directive_name)
    {
        let Some(methods_object) = methods_value.as_object() else {
            continue;
        };
        for (method_name, method_value) in methods_object {
            if let Some(body) = method_value.as_str()
                && let Ok(method) = CompiledMethod::parse(method_name.as_str(), body, spec)
            {
                compiled.push(method);
            }
        }
    }

    // `@method`-derived nullary methods.
    compiled.extend(crate::connectors::spec::method::compiled_derived_methods(
        schema,
        method_directive_name,
        spec,
    ));

    if compiled.is_empty() {
        return None;
    }
    MethodRegistry::build(compiled).ok().map(Arc::new)
}

/// Validate `@method` directives: version gate, `selection:` parse, and the
/// v1 auto-derive placeholder (object-typed fields need an explicit
/// `selection:`). The registry invariants (duplicate names vs `@source(methods:)`,
/// cycles) are covered by [`validate`] over the merged registry.
pub(super) fn validate_method_directives(schema: &SchemaInfo) -> Vec<Message> {
    let method_directive = &schema.connect_link.method_directive_name;
    let spec = schema.connect_link.spec;
    let sources = &schema.schema.sources;
    let mut messages = Vec::new();

    for ty in schema.schema.types.values() {
        let (directives, fields) = match ty {
            ExtendedType::Object(obj) => (&obj.directives, &obj.fields),
            ExtendedType::Interface(iface) => (&iface.directives, &iface.fields),
            _ => continue,
        };

        for directive in directives.iter().filter(|d| d.name == *method_directive) {
            let locations: Vec<_> = directive.line_column_range(sources).into_iter().collect();

            // 1. Version gate.
            if spec < ConnectSpec::V0_5 {
                messages.push(Message {
                    code: Code::MethodDirectiveRequiresV0_5,
                    message: format!(
                        "`@{method_directive}` requires connect spec v0.5 or later, but this schema links connect v{spec}."
                    ),
                    locations,
                });
                continue;
            }

            match directive
                .specified_argument_by_name(CONNECT_SELECTION_ARGUMENT_NAME.as_str())
                .and_then(|value| value.as_str())
            {
                // 2. Explicit selection: must parse as a method body.
                Some(selection) => {
                    if let Err(err) = CompiledMethod::parse("__mapping__", selection, spec) {
                        messages.push(Message {
                            code: Code::InvalidMethod,
                            message: format!(
                                "`@{method_directive}(selection:)` is not a valid mapping: {}",
                                err.message
                            ),
                            locations,
                        });
                    }
                }
                // 3. Auto-derive: object-typed fields are the parked co-design
                //    item — require an explicit `selection:` for now.
                None => {
                    if let Some((field_name, _)) = fields.iter().find(|(_, field)| {
                        matches!(
                            schema.schema.types.get(field.ty.inner_named_type()),
                            Some(
                                ExtendedType::Object(_)
                                    | ExtendedType::Interface(_)
                                    | ExtendedType::Union(_)
                            )
                        )
                    }) {
                        messages.push(Message {
                            code: Code::UnsupportedMethodAutoDerive,
                            message: format!(
                                "`@{method_directive}` cannot auto-derive a mapping for the object-typed field `{field_name}`; provide an explicit `selection:`."
                            ),
                            locations,
                        });
                    }
                }
            }
        }
    }

    messages
}

/// Validate every `@source(methods:)` and `@connect(methods:)` block in the schema.
pub(super) fn validate(schema: &SchemaInfo) -> Vec<Message> {
    let source_directive_name = schema.source_directive_name();
    let connect_directive_name = schema.connect_directive_name();
    let spec = schema.connect_link.spec;
    let sources = &schema.schema.sources;

    let mut messages = Vec::new();
    let mut compiled = Vec::new();
    // Locations of the `methods:` argument(s), used to anchor global-registry
    // errors (which span every declaration site, so no single body location
    // fits).
    let mut methods_locations = Vec::new();

    for (directive_name, methods_value) in
        methods_arguments(schema.schema, source_directive_name, connect_directive_name)
    {
        let arg_locations: Vec<_> = methods_value
            .line_column_range(sources)
            .into_iter()
            .collect();
        methods_locations.extend(arg_locations.iter().cloned());

        // 1. Version gate.
        if spec < ConnectSpec::V0_5 {
            messages.push(Message {
                code: Code::MethodsArgumentRequiresV0_5,
                message: format!(
                    "`@{directive_name}(methods:)` requires connect spec v0.5 or later, but this schema links connect v{spec}."
                ),
                locations: arg_locations,
            });
            continue;
        }

        let Some(methods_object) = methods_value.as_object() else {
            messages.push(Message {
                code: Code::InvalidMethod,
                message: format!(
                    "`@{directive_name}(methods:)` must be an object mapping method names to mapping bodies."
                ),
                locations: arg_locations,
            });
            continue;
        };

        // 2. Per-body parse.
        for (method_name, method_value) in methods_object {
            let value_locations: Vec<_> = method_value
                .line_column_range(sources)
                .into_iter()
                .collect();
            let Some(body) = method_value.as_str() else {
                // v1: string-only leaves; object leaves are reserved for future
                // long-form config.
                messages.push(Message {
                    code: Code::InvalidMethod,
                    message: format!(
                        "`@{directive_name}(methods:)` entry `{method_name}` must be a string mapping body."
                    ),
                    locations: value_locations,
                });
                continue;
            };
            match CompiledMethod::parse(method_name.as_str(), body, spec) {
                Ok(method) => compiled.push(method),
                Err(err) => messages.push(Message {
                    code: Code::InvalidMethod,
                    message: format!(
                        "`@{directive_name}(methods:)` entry `{method_name}` is not a valid mapping: {}",
                        err.message
                    ),
                    locations: value_locations,
                }),
            }
        }
    }

    // 3. `@method`-derived methods occupy the same namespace, so they have to be
    // part of the set the invariants below are checked over. Leaving them out
    // let a name declared by both a `@method` and a `methods:` block pass
    // composition and then fail *extraction* as
    // `FederationError::internal("… should have been caught in validation")` —
    // an internal error where the author should have seen `DuplicateMethod`.
    //
    // Guarded on the version gate: under v0.5 `@method` use is already reported
    // by `validate_method_directives`, and folding those methods in there would pile
    // duplicate errors on top of the real one.
    if spec >= ConnectSpec::V0_5 {
        let method_directive_name = &schema.connect_link.method_directive_name;
        compiled.extend(crate::connectors::spec::method::compiled_derived_methods(
            schema.schema,
            method_directive_name,
            spec,
        ));
        // Anchor registry errors at the `@method` directives too, so a
        // `@method`/`methods:` collision points at both declaration sites.
        methods_locations.extend(
            applied_directives(schema.schema)
                .into_iter()
                .filter(|directive| directive.name == *method_directive_name)
                // `applied_directives` hands back `&Directive`, which has no
                // range of its own; the name node carries one.
                .flat_map(|directive| directive.name.line_column_range(sources)),
        );
    }

    // 4. Advisory: a method that shadows an ordinary built-in is legal and wins
    // (see `MethodRegistry`), but the author should know a built-in of that name
    // exists — otherwise they stay on their own implementation forever without
    // ever being told ours arrived. `Code::MethodShadowsBuiltin` is a
    // `Severity::Warning`, so this does not fail composition. Reserved names are
    // a different, fatal case and are reported by the registry build below.
    for method in &compiled {
        if is_builtin_method_name(&method.name) && !is_reserved_method_name(&method.name) {
            messages.push(Message {
                code: Code::MethodShadowsBuiltin,
                message: format!(
                    "Custom `->` method `{name}` shadows the built-in `->{name}`, and takes precedence over it wherever it is called. This is allowed — remove the `methods:` entry if you would rather use the built-in.",
                    name = method.name
                ),
                locations: methods_locations.clone(),
            });
        }
    }

    // 5. Global registry invariants. Only build if every body parsed — a parse
    // failure already produced a message, and feeding a partial set to the
    // registry would just add confusing follow-on errors.
    if !messages
        .iter()
        .any(|m| m.code.severity() == Severity::Error)
        && !compiled.is_empty()
        && let Err(errors) = MethodRegistry::build(compiled)
    {
        for error in errors {
            messages.push(method_error_message(error, &methods_locations));
        }
    }

    messages
}

/// Reject `->method` calls that name neither a built-in nor a declared method.
///
/// Without this, a typo like `->fisrt` composes cleanly and only fails per
/// request, at which point the directive silently yields nothing useful. The set
/// of resolvable names is exactly *built-ins ∪ declared methods*, which is only
/// knowable once the method registry exists — hence this living beside the method
/// validation rather than in the selection type-checker.
///
/// Every selection-valued argument connectors define is checked, including method
/// bodies (a method may call another method, or fat-finger a built-in). Coordinates
/// are the argument, not the offset inside the directive string; the message names
/// the method so it is still unambiguous.
pub(super) fn validate_method_calls(schema: &SchemaInfo) -> Vec<Message> {
    let spec = schema.connect_link.spec;
    let sources = &schema.schema.sources;
    let source_directive_name = schema.source_directive_name();
    let connect_directive_name = schema.connect_directive_name();
    let method_directive_name = &schema.connect_link.method_directive_name;

    // If the registry failed to assemble (duplicate, reserved shadow, cycle),
    // `schema.methods()` is `None` and every custom call would look unknown. Those
    // errors are already reported; staying quiet here avoids burying them under
    // a pile of misleading "unknown method" follow-ons.
    let registry = schema.methods();
    let declared = |name: &str| registry.is_some_and(|registry| registry.get(name).is_some());

    // Fatal only for the spec version that introduces the check. v0.1–v0.4
    // shipped without it, so a schema carrying a typo'd method composes and
    // deploys today; failing it now would break that graph on upgrade. Warn
    // there instead.
    let unknown_method_code = if spec >= ConnectSpec::V0_5 {
        Code::UnknownMethod
    } else {
        Code::UnknownMethodLegacySpec
    };

    // Bound as locals so `as_str()` borrows live for the whole function; the
    // `const` names would otherwise produce per-use temporaries.
    let selection_arg = CONNECT_SELECTION_ARGUMENT_NAME;
    let is_success_arg = IS_SUCCESS_ARGUMENT_NAME;

    // (directive name, argument name, value) for every directive-valued argument.
    let mut selections: Vec<(&Name, &Name, &Node<Value>)> = Vec::new();

    for directive in &schema.schema_definition.directives {
        if directive.name != *source_directive_name {
            continue;
        }
        if let Some(value) = directive.specified_argument_by_name(is_success_arg.as_str()) {
            selections.push((source_directive_name, &is_success_arg, value));
        }
    }

    for directive in applied_directives(schema.schema) {
        let (directive_name, args): (&Name, &[&Name]) = if directive.name == *connect_directive_name
        {
            (connect_directive_name, &[&selection_arg, &is_success_arg])
        } else if directive.name == *method_directive_name {
            (method_directive_name, &[&selection_arg])
        } else {
            continue;
        };
        for arg in args {
            if let Some(value) = directive.specified_argument_by_name(arg.as_str()) {
                selections.push((directive_name, arg, value));
            }
        }
    }

    let mut messages = Vec::new();

    // Takes an already-parsed selection rather than a string: a method body may open
    // with a `($a, $b) =>` parameter header, which only `CompiledMethod::parse`
    // accepts. Parsing here with `JSONSelection::parse_with_spec` would fail on
    // every parameterized method and silently skip it.
    let mut check = |parsed: &JSONSelection,
                     locations: Vec<Range<LineColumn>>,
                     coordinate: String| {
        for method in parsed.method_calls() {
            let name = method.as_ref().as_str();
            if is_builtin_method_name(name) || declared(name) {
                continue;
            }
            messages.push(Message {
                code: unknown_method_code,
                message: if spec >= ConnectSpec::V0_5 {
                    format!(
                        "`{coordinate}` calls `->{name}`, which is not a built-in method and is not declared in any `methods:`. Check the spelling, or declare it via `@{source_directive_name}(methods:)` or `@{connect_directive_name}(methods:)`."
                    )
                } else {
                    // `methods:` does not exist before v0.5, so suggesting it here
                    // would be advice the author cannot act on.
                    format!(
                        "`{coordinate}` calls `->{name}`, which is not a built-in method. Check the spelling — this call will produce no value at request time."
                    )
                },
                locations: locations.clone(),
            });
        }
    };

    for (directive_name, arg_name, value) in selections {
        let Some(body) = value.as_str() else {
            continue;
        };
        // A body that does not parse is reported by whoever owns it.
        let Ok(parsed) = JSONSelection::parse_with_spec(body, spec) else {
            continue;
        };
        let locations: Vec<_> = value.line_column_range(sources).into_iter().collect();
        check(
            &parsed,
            locations,
            format!("@{directive_name}({arg_name}:)"),
        );
    }

    // Def bodies, which are mappings too — parsed as methods so a parameter header
    // is accepted rather than making the whole body unparseable and unchecked.
    for (directive_name, methods_value) in
        methods_arguments(schema.schema, source_directive_name, connect_directive_name)
    {
        let Some(methods_object) = methods_value.as_object() else {
            continue;
        };
        for (method_name, method_value) in methods_object {
            let Some(body) = method_value.as_str() else {
                continue;
            };
            let Ok(method) = CompiledMethod::parse(method_name.as_str(), body, spec) else {
                continue;
            };
            let locations: Vec<_> = method_value
                .line_column_range(sources)
                .into_iter()
                .collect();
            check(
                &method.body,
                locations,
                format!("@{directive_name}(methods: {{ {method_name}: }})"),
            );
        }
    }

    messages
}

/// Phrase a registry-level error. These are deliberately *not* attributed to one
/// directive: the invariant they enforce spans every `@source(methods:)`,
/// `@connect(methods:)`, and `@method` in the subgraph, and naming just one of
/// them would suggest the namespace is narrower than it is. `locations` covers
/// all declaration sites so the author can see the whole conflict.
fn method_error_message(error: MethodError, locations: &[Range<LineColumn>]) -> Message {
    let locations = locations.to_vec();
    match error {
        MethodError::DuplicateName { name } => Message {
            code: Code::DuplicateMethod,
            message: format!(
                "Duplicate custom `->` method name `{name}` in `methods:`. Each method must have a unique name across the whole subgraph — `@source(methods:)` and `@connect(methods:)` share one namespace, so declaring a method on a connector does not scope it to that connector."
            ),
            locations,
        },
        MethodError::ShadowsReserved { name } => Message {
            code: Code::MethodShadowsReserved,
            message: format!(
                "Custom `->` method `{name}` cannot be defined: `->{name}` is reserved. Unlike ordinary built-ins, its meaning is fixed by the directive language itself (it is interpreted at parse time), so a definition cannot replace it. Choose a different name."
            ),
            locations,
        },
        MethodError::NotInlinable { cycle } => Message {
            code: Code::NonInlinableMethods,
            message: format!(
                "These custom `->` methods cannot be expanded because they refer to one another in a cycle: {}. Custom `->` methods must be fully inlinable.",
                cycle.join(" -> ")
            ),
            locations,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::super::Code;
    use super::super::Severity;
    use super::super::validate;

    /// Build a minimal connectors schema linking the given connect version,
    /// with a single `@source` carrying the provided `methods:` object literal.
    fn schema_with_methods(version: &str, methods_literal: &str) -> String {
        format!(
            r#"extend schema
@link(url: "https://specs.apollo.dev/connect/v{version}", import: ["@connect", "@source"])
@source(name: "v", http: {{ baseURL: "http://localhost" }}, methods: {methods_literal})

type Query {{
  f: String @connect(source: "v", http: {{ GET: "/" }}, selection: "$")
}}
"#
        )
    }

    fn codes(schema: String) -> Vec<Code> {
        validate(schema, "test.graphql")
            .errors
            .into_iter()
            .map(|m| m.code)
            .collect()
    }

    #[test]
    fn methods_argument_under_v0_4_is_version_gated() {
        let schema = schema_with_methods("0.4", r#"{ User: "id name" }"#);
        assert!(
            codes(schema).contains(&Code::MethodsArgumentRequiresV0_5),
            "expected MethodsArgumentRequiresV0_5 under connect v0.4"
        );
    }

    #[test]
    fn valid_defs_under_v0_5_produce_no_def_errors() {
        let schema = schema_with_methods("0.5", r#"{ User: "id name" }"#);
        let codes = codes(schema);
        let method_codes = [
            Code::MethodsArgumentRequiresV0_5,
            Code::InvalidMethod,
            Code::DuplicateMethod,
            Code::NonInlinableMethods,
        ];
        assert!(
            !codes.iter().any(|c| method_codes.contains(c)),
            "valid v0.5 methods should produce no method-related errors, got {codes:?}"
        );
    }

    #[test]
    fn cyclic_defs_are_rejected_under_v0_5() {
        let schema = schema_with_methods("0.5", r#"{ a: "$->b", b: "$->a" }"#);
        assert!(
            codes(schema).contains(&Code::NonInlinableMethods),
            "expected NonInlinableMethods for a -> b -> a cycle"
        );
    }

    #[test]
    fn builtin_shadow_is_allowed_under_v0_5() {
        // Shadowing a built-in must NOT be an error: authors define a custom
        // method precisely when a desirable built-in is missing, and they pick
        // the obvious name for it. If shadowing were rejected, later shipping a
        // built-in of that name would break schemas that compose today. See
        // `MethodRegistry`.
        let schema = schema_with_methods("0.5", r#"{ map: "id" }"#);
        let codes = codes(schema);
        assert!(
            !codes
                .iter()
                .any(|c| matches!(c, Code::InvalidMethod | Code::DuplicateMethod)),
            "a method named `map` should compose, got {codes:?}"
        );
    }

    #[test]
    fn malformed_def_body_is_rejected_under_v0_5() {
        // `($a)` opens a parameter header but never closes it with `=>`.
        let schema = schema_with_methods("0.5", r#"{ broken: "($a) $a" }"#);
        assert!(
            codes(schema).contains(&Code::InvalidMethod),
            "expected InvalidMethod for a malformed body"
        );
    }

    #[test]
    fn duplicate_method_names_across_sources_are_rejected() {
        let schema = r#"extend schema
@link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@connect", "@source"])
@source(name: "a", http: { baseURL: "http://localhost" }, methods: { foo: "id" })
@source(name: "b", http: { baseURL: "http://localhost" }, methods: { foo: "name" })

type Query {
  f: String @connect(source: "a", http: { GET: "/" }, selection: "$")
}
"#
        .to_string();
        assert!(
            codes(schema).contains(&Code::DuplicateMethod),
            "expected DuplicateMethod for `foo` declared on two sources"
        );
    }

    #[test]
    fn unknown_method_is_an_error_under_v0_5() {
        let schema = r#"extend schema
@link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@connect", "@source"])
@source(name: "v", http: { baseURL: "http://localhost" })

type Query {
  a: String @connect(source: "v", http: { GET: "/a" }, selection: "$->neverDeclared")
}
"#
        .to_string();
        assert!(
            codes(schema).contains(&Code::UnknownMethod),
            "a call to an undeclared `->method` should fail composition under v0.5"
        );
    }

    #[test]
    fn unknown_method_is_only_a_warning_on_released_specs() {
        // v0.1–v0.4 shipped without this check, so schemas carrying a typo'd
        // method compose and deploy today. Reporting it is useful; failing it
        // would break those graphs on upgrade.
        let schema = r#"extend schema
@link(url: "https://specs.apollo.dev/connect/v0.4", import: ["@connect", "@source"])
@source(name: "v", http: { baseURL: "http://localhost" })

type Query {
  a: String @connect(source: "v", http: { GET: "/a" }, selection: "$->neverDeclared")
}
"#
        .to_string();
        let codes = codes(schema);
        assert!(
            codes.contains(&Code::UnknownMethodLegacySpec),
            "expected the non-fatal variant under v0.4, got {codes:?}"
        );
        assert!(
            !codes.contains(&Code::UnknownMethod),
            "the fatal variant must not fire on a released spec, got {codes:?}"
        );
        assert_eq!(
            Code::UnknownMethodLegacySpec.severity(),
            Severity::Warning,
            "the legacy variant must not fail composition"
        );
    }

    #[test]
    fn a_declared_def_is_not_an_unknown_method() {
        // The flip side: the check must not fire on legitimately declared methods,
        // whichever directive declared them.
        let schema = r#"extend schema
@link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@connect", "@source"])
@source(name: "v", http: { baseURL: "http://localhost" }, methods: { fromSource: "id" })

type Query {
  a: String @connect(source: "v", http: { GET: "/a" }, selection: "$->fromSource", methods: { fromConnect: "id" })
  b: String @connect(source: "v", http: { GET: "/b" }, selection: "$->fromConnect")
}
"#
        .to_string();
        let codes = codes(schema);
        assert!(
            !codes.contains(&Code::UnknownMethod),
            "declared methods must resolve, got {codes:?}"
        );
    }

    #[test]
    fn unknown_method_inside_a_def_body_is_caught() {
        // A method body is a directive too, so a typo inside one must be reported.
        let schema = schema_with_methods("0.5", r#"{ outer: "$->typoInside" }"#);
        assert!(
            codes(schema).contains(&Code::UnknownMethod),
            "expected UnknownMethod for a bad call inside a method body"
        );
    }

    #[test]
    fn shadowing_an_ordinary_builtin_warns() {
        let schema = schema_with_methods("0.5", r#"{ map: "id" }"#);
        let codes = codes(schema);
        assert!(
            codes.contains(&Code::MethodShadowsBuiltin),
            "expected a shadowing advisory for a method named `map`, got {codes:?}"
        );
        assert_eq!(
            Code::MethodShadowsBuiltin.severity(),
            Severity::Warning,
            "shadowing an ordinary built-in must not fail composition"
        );
    }

    #[test]
    fn shadowing_a_reserved_method_is_an_error() {
        // `->as` is interpreted at parse time (it decides which names become
        // bound variables), so a method cannot replace it without making parse-time
        // and eval-time disagree. Unlike ordinary built-ins, reserving it cannot
        // retroactively break a schema, because it already means this today.
        let schema = schema_with_methods("0.5", r#"{ as: "id" }"#);
        assert!(
            codes(schema).contains(&Code::MethodShadowsReserved),
            "expected MethodShadowsReserved for a method named `as`"
        );
    }

    #[test]
    fn connect_defs_under_v0_4_is_version_gated() {
        let schema = r#"extend schema
@link(url: "https://specs.apollo.dev/connect/v0.4", import: ["@connect", "@source"])
@source(name: "v", http: { baseURL: "http://localhost" })

type Query {
  f: String @connect(source: "v", http: { GET: "/" }, selection: "$", methods: { User: "id name" })
}
"#
        .to_string();
        assert!(
            codes(schema).contains(&Code::MethodsArgumentRequiresV0_5),
            "expected MethodsArgumentRequiresV0_5 for `@connect(methods:)` under connect v0.4"
        );
    }

    #[test]
    fn method_declared_on_one_connect_is_visible_to_every_connector() {
        // The colocation contract, asserted positively: `@connect(methods:)` places
        // a method next to the connector that motivated it but does NOT scope it
        // there, so *every* connector in the subgraph — including ones that
        // declared nothing — must receive it in its registry.
        //
        // This checks the extracted registry rather than the absence of
        // validation errors, because compose-time validation does not currently
        // reject calls to undeclared `->methods` at all (true on `dev` too,
        // independent of methods) — so an absence-of-errors assertion here would
        // pass even if `@connect(methods:)` were ignored entirely.
        let schema = apollo_compiler::Schema::parse(
            r#"extend schema
@link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@connect", "@source"])
@source(name: "v", http: { baseURL: "http://localhost" })

type Query {
  a: User @connect(source: "v", http: { GET: "/a" }, selection: "$->User", methods: { User: "id name" })
  b: User @connect(source: "v", http: { GET: "/b" }, selection: "$->User")
}

type User {
  id: ID
  name: String
}
"#,
            "test.graphql",
        )
        .unwrap();

        let connectors = crate::connectors::Connector::from_schema(&schema, "test").unwrap();
        assert_eq!(connectors.len(), 2, "expected one connector per field");
        for connector in &connectors {
            let registry = connector
                .methods
                .as_ref()
                .unwrap_or_else(|| panic!("{} has no method registry", connector.label.0));
            assert!(
                registry.get("User").is_some(),
                "`User` should be visible to {}, which did not declare it",
                connector.label.0
            );
        }
    }

    #[test]
    fn duplicate_method_names_across_two_connects_are_rejected() {
        // Corollary of the above: because there is one namespace, two connectors
        // cannot each declare their own `foo`. If this ever stops failing, methods
        // have silently become connector-scoped.
        let schema = r#"extend schema
@link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@connect", "@source"])
@source(name: "v", http: { baseURL: "http://localhost" })

type Query {
  a: String @connect(source: "v", http: { GET: "/a" }, selection: "$", methods: { foo: "id" })
  b: String @connect(source: "v", http: { GET: "/b" }, selection: "$", methods: { foo: "name" })
}
"#
        .to_string();
        assert!(
            codes(schema).contains(&Code::DuplicateMethod),
            "expected DuplicateMethod for `foo` declared on two connectors"
        );
    }

    #[test]
    fn duplicate_method_name_across_source_and_connect_is_rejected() {
        let schema = r#"extend schema
@link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@connect", "@source"])
@source(name: "v", http: { baseURL: "http://localhost" }, methods: { foo: "id" })

type Query {
  a: String @connect(source: "v", http: { GET: "/a" }, selection: "$", methods: { foo: "name" })
}
"#
        .to_string();
        assert!(
            codes(schema).contains(&Code::DuplicateMethod),
            "expected DuplicateMethod for `foo` declared on both a source and a connector"
        );
    }

    #[test]
    fn documented_example_composes() {
        // The worked example from the PR description and CNN-1107, kept executable
        // so it cannot drift from what the code actually accepts. Exercises every
        // declaration form at once: a parameterized method on `@source`, a nullary
        // method colocated on a `@connect`, a `@method`-derived method reused by two
        // connectors, and a virtual (requestless) connector calling a method over
        // `$args`.
        let schema = r#"extend schema
  @link(
    url: "https://specs.apollo.dev/connect/v0.5"
    import: ["@connect", "@source", "@method"]
  )
  @source(
    name: "api"
    http: { baseURL: "https://api.example.com" }
    methods: {
      min: "($other) => @->as($self)->lte($other)->match([true, $self], [@, $other])"
      max: "($other) => @->as($self)->gte($other)->match([true, $self], [@, $other])"
    }
  )

type User @method {
  id: ID!
  name: String
  email: String
}

type Query {
  user: User @connect(source: "api", http: { GET: "/user" }, selection: "$->User")

  team: [User] @connect(source: "api", http: { GET: "/team" }, selection: "$->map(@->User)")

  clamp(input: Int!, low: Int!, high: Int!): Int!
    @connect(
      selection: "$args.input->clamp($args.low, $args.high)"
      methods: { clamp: "($lo, $hi) => @->min($hi)->max($lo)" }
    )
}
"#
        .to_string();
        let errors = validate(schema, "test.graphql").errors;
        assert!(
            errors.is_empty(),
            "the documented example must compose cleanly, got: {errors:?}"
        );
    }

    #[test]
    fn unknown_method_inside_a_parameterized_def_body_is_caught() {
        // A method body may open with a `($a, $b) =>` parameter header, which plain
        // `JSONSelection::parse_with_spec` rejects. Checking bodies as strings
        // therefore skipped every parameterized method silently — the nullary test
        // above passed while this case went unchecked.
        let schema = schema_with_methods("0.5", r#"{ clamp: "($lo, $hi) => @->typoInside($hi)" }"#);
        assert!(
            codes(schema).contains(&Code::UnknownMethod),
            "expected UnknownMethod for a bad call inside a parameterized method body"
        );
    }

    #[test]
    fn duplicate_method_name_across_method_directive_and_methods_argument_is_rejected() {
        // `@method` is the third declaration form, so it must obey the same
        // one-namespace rule: a `@method`-derived method named after its type
        // collides with a `methods:` entry of that name. Without this the rule would
        // be tested for two of the three pairs only.
        let schema = r#"extend schema
@link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@connect", "@source", "@method"])
@source(name: "v", http: { baseURL: "http://localhost" }, methods: { User: "id" })

type Query {
  a: User @connect(source: "v", http: { GET: "/a" }, selection: "$->User")
}

type User @method {
  id: ID
  name: String
}
"#
        .to_string();
        assert!(
            codes(schema).contains(&Code::DuplicateMethod),
            "expected DuplicateMethod for `User` declared by both `@method` and `methods:`"
        );
    }

    #[test]
    fn method_directive_auto_derives_a_nullary_method_over_all_fields() {
        // Bare `@method` — no `selection:`, no `as:` — registers a nullary method
        // named after the type whose body selects every field, callable as
        // `$->User` with no arguments. Asserted against the extracted registry
        // rather than the absence of errors, so it cannot pass vacuously.
        let schema = apollo_compiler::Schema::parse(
            r#"extend schema
@link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@connect", "@source", "@method"])
@source(name: "v", http: { baseURL: "http://localhost" })

type Query {
  user: User @connect(source: "v", http: { GET: "/u" }, selection: "$->User")
}

type User @method {
  id: ID
  name: String
  email: String
}
"#,
            "test.graphql",
        )
        .unwrap();

        let connectors = crate::connectors::Connector::from_schema(&schema, "test").unwrap();
        let registry = connectors
            .first()
            .expect("one connector")
            .methods
            .as_ref()
            .expect("a `@method` should populate the registry");
        let method = registry
            .get("User")
            .expect("`@method` should register a method named after its type");
        assert!(
            method.params.is_empty(),
            "an auto-derived directive takes no arguments, got {:?}",
            method.params
        );
        assert_eq!(method.body.to_string(), "id\nname\nemail");
    }

    #[test]
    fn connect_defs_on_a_type_are_registered() {
        // `@connect` is also valid on OBJECT; methods declared there feed the same
        // registry, and a malformed body must still be reported.
        let schema = r#"extend schema
@link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@connect", "@source"])
@source(name: "v", http: { baseURL: "http://localhost" })

type Query {
  t: Thing
}

type Thing @connect(source: "v", http: { GET: "/t" }, selection: "id", methods: { broken: "($a) $a" }) {
  id: ID
}
"#
        .to_string();
        assert!(
            codes(schema).contains(&Code::InvalidMethod),
            "expected InvalidMethod for a malformed body in `@connect(methods:)` on a type"
        );
    }

    #[test]
    fn method_used_in_connect_selection_composes() {
        // End-to-end: a custom method invoked in `@connect(selection:)` must
        // resolve during shape-based validation, not fail as "method not
        // found". The shape pass threads the registry via SchemaInfo.
        let schema = r#"extend schema
@link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@connect", "@source"])
@source(name: "v", http: { baseURL: "http://localhost" }, methods: { User: "id name" })

type Query {
  user: User @connect(source: "v", http: { GET: "/u" }, selection: "$->User")
}

type User {
  id: ID
  name: String
}
"#
        .to_string();
        let errors = validate(schema, "test.graphql").errors;
        assert!(
            !errors.iter().any(|m| m.message.contains("not found")),
            "custom method in selection should resolve, got: {errors:?}"
        );
        assert!(
            !errors.iter().any(|m| matches!(
                m.code,
                Code::InvalidSelection
                    | Code::MethodsArgumentRequiresV0_5
                    | Code::InvalidMethod
                    | Code::NonInlinableMethods
            )),
            "expected a clean compose for a method-using selection, got: {errors:?}"
        );
    }

    #[test]
    fn mapping_used_in_selection_composes() {
        // `@method` auto-derives a nullary method named after the type; invoking
        // it in a selection resolves end-to-end.
        let schema = r#"extend schema
@link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@connect", "@source", "@method"])
@source(name: "v", http: { baseURL: "http://localhost" })

type Query {
  user: User @connect(source: "v", http: { GET: "/u" }, selection: "$->User")
}

type User @method {
  id: ID
  name: String
}
"#
        .to_string();
        let errors = validate(schema, "test.graphql").errors;
        assert!(
            !errors.iter().any(|m| m.message.contains("not found")
                || matches!(
                    m.code,
                    Code::MethodDirectiveRequiresV0_5
                        | Code::UnsupportedMethodAutoDerive
                        | Code::InvalidSelection
                )),
            "expected a clean compose for a @method-derived selection, got: {errors:?}"
        );
    }

    #[test]
    fn mapping_under_v0_4_is_version_gated() {
        let schema = r#"extend schema
@link(url: "https://specs.apollo.dev/connect/v0.4", import: ["@connect", "@source", "@method"])
@source(name: "v", http: { baseURL: "http://localhost" })

type Query {
  user: User @connect(source: "v", http: { GET: "/u" }, selection: "id")
}

type User @method {
  id: ID
}
"#
        .to_string();
        assert!(
            codes(schema).contains(&Code::MethodDirectiveRequiresV0_5),
            "expected MethodDirectiveRequiresV0_5 under connect v0.4"
        );
    }

    #[test]
    fn mapping_auto_derive_over_object_field_is_rejected() {
        // Parked co-design item: auto-derive can't yet handle an object-typed
        // field; require an explicit `selection:`.
        let schema = r#"extend schema
@link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@connect", "@source", "@method"])
@source(name: "v", http: { baseURL: "http://localhost" })

type Query {
  parent: Parent @connect(source: "v", http: { GET: "/p" }, selection: "$->Parent")
}

type Parent @method {
  id: ID
  child: Child
}

type Child {
  x: Int
}
"#
        .to_string();
        assert!(
            codes(schema).contains(&Code::UnsupportedMethodAutoDerive),
            "expected UnsupportedMethodAutoDerive for an object-typed field"
        );
    }
}
