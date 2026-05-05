//! MappingRegistry for storing and expanding @mapping directive definitions.
//!
//! This module provides the `MappingRegistry` which stores parsed mapping definitions
//! and can expand `...TypeName` spread syntax in JSONSelection strings.

use std::collections::HashMap;
use std::collections::HashSet;

/// Maximum depth for mapping expansion to prevent stack overflow on deeply nested chains.
/// This is intentionally conservative - typical use cases have 1-3 levels of nesting.
const MAX_EXPANSION_DEPTH: usize = 32;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use indexmap::IndexMap;

use super::ConnectSpec;
use super::JSONSelection;
use super::json_selection::KnownVariable;
use super::json_selection::LitExpr;
use super::json_selection::MethodArgs;
use super::json_selection::NamedSelection;
use super::json_selection::NamingPrefix;
use super::json_selection::PathList;
use super::json_selection::PathSelection;
use super::json_selection::Ranged;
use super::json_selection::SpreadArgs;
use super::json_selection::SubSelection;
use super::json_selection::TopLevelSelection;
use super::json_selection::VarPaths;
use super::json_selection::WithRange;
use super::spec::ConnectLink;
use super::spec::MappingDirectiveArguments;
use super::spec::extract_mapping_directive_arguments;
use super::variable::Namespace;
use crate::error::FederationError;

/// A parsed mapping definition from a `@mapping` directive
#[derive(Debug, Clone)]
pub struct MappingDefinition {
    /// The parsed selection for this mapping
    pub(crate) selection: TopLevelSelection,
    /// The original GraphQL type this mapping is defined on
    pub source_type: Name,
    /// Parameter names inferred from the selection (external variables that are
    /// NOT known runtime namespaces like $this, $args, etc.)
    pub(crate) parameters: HashSet<String>,
}

/// Registry of all @mapping definitions in a schema
#[derive(Debug, Clone, Default)]
pub struct MappingRegistry {
    /// Mappings keyed by their alias (or type name if no alias)
    mappings: IndexMap<Name, MappingDefinition>,
}

impl MappingRegistry {
    /// Create a new empty registry
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry from a schema by extracting all @mapping directives
    pub fn from_schema(schema: &Schema) -> Result<Self, FederationError> {
        // Get the mapping directive name and spec from the ConnectLink
        let (directive_name, spec) = match ConnectLink::new(schema) {
            Some(Ok(link)) => (link.mapping_directive_name.clone(), link.spec),
            Some(Err(e)) => {
                // Propagate errors from ConnectLink creation (e.g., unknown spec version)
                return Err(FederationError::internal(e.message));
            }
            None => {
                // No connect link at all, return empty registry
                return Ok(Self::default());
            }
        };

        let mut registry = Self::new();

        // Extract all @mapping directive arguments
        let mapping_args = extract_mapping_directive_arguments(schema, &directive_name)?;

        for args in mapping_args {
            let definition = Self::build_mapping_definition(&args, spec)?;
            registry.mappings.insert(args.alias.clone(), definition);
        }

        Ok(registry)
    }

    /// Build a MappingDefinition from directive arguments
    fn build_mapping_definition(
        args: &MappingDirectiveArguments,
        spec: ConnectSpec,
    ) -> Result<MappingDefinition, FederationError> {
        let selection = if let Some(selection_str) = &args.selection {
            // Explicit selection - parse it using the schema's actual spec version
            let parsed = JSONSelection::parse_with_spec(selection_str, spec).map_err(|e| {
                FederationError::internal(format!(
                    "Failed to parse @mapping selection on type `{}`: {}",
                    args.type_name, e
                ))
            })?;

            parsed.inner
        } else {
            // Auto-map mode: generate selection from field names
            Self::generate_auto_map_selection(&args.type_name, &args.field_names, spec)?
        };

        let parameters = Self::compute_parameters(&selection);

        Ok(MappingDefinition {
            selection,
            source_type: args.type_name.clone(),
            parameters,
        })
    }

    /// Compute the set of parameter names from a mapping's selection.
    /// Parameters are external variables ($name) that are NOT known runtime namespaces.
    fn compute_parameters(selection: &TopLevelSelection) -> HashSet<String> {
        use std::str::FromStr;

        let var_paths = match selection {
            TopLevelSelection::Named(sub) => sub.var_paths(),
            TopLevelSelection::Path(path) => path.var_paths(),
        };

        var_paths
            .into_iter()
            .filter_map(|var_path| {
                if let PathList::Var(known_var, _) = var_path.path.as_ref()
                    && let KnownVariable::External(name) = known_var.as_ref()
                    && Namespace::from_str(name).is_err()
                {
                    // Strip the leading '$' for the parameter name
                    Some(name.trim_start_matches('$').to_string())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Generate an auto-map selection from field names.
    ///
    /// Builds a selection string like `"field1 field2 field3"` from the type's
    /// field names and parses it into a `TopLevelSelection`.
    fn generate_auto_map_selection(
        type_name: &Name,
        field_names: &[Name],
        spec: ConnectSpec,
    ) -> Result<TopLevelSelection, FederationError> {
        let selection_str = field_names
            .iter()
            .map(|n| n.as_str())
            .collect::<Vec<_>>()
            .join(" ");

        if selection_str.is_empty() {
            return Err(FederationError::internal(format!(
                "@mapping on type `{type_name}` has no fields to auto-map",
            )));
        }

        let parsed = JSONSelection::parse_with_spec(&selection_str, spec).map_err(|e| {
            FederationError::internal(format!(
                "Failed to generate auto-map selection for type `{type_name}`: {e}",
            ))
        })?;

        match parsed.inner {
            TopLevelSelection::Named(sub) => Ok(TopLevelSelection::Named(sub)),
            TopLevelSelection::Path(_) => Err(FederationError::internal(format!(
                "Auto-map for type `{type_name}` generated unexpected path selection",
            ))),
        }
    }

    /// Check if the registry has a mapping with the given name
    pub fn has_mapping(&self, name: &str) -> bool {
        self.mappings.contains_key(name)
    }

    /// Get a mapping by name
    pub fn get_mapping(&self, name: &str) -> Option<&MappingDefinition> {
        self.mappings.get(name)
    }

    /// Insert a mapping directly (for testing and programmatic construction)
    #[cfg(test)]
    pub fn insert_mapping(&mut self, name: Name, definition: MappingDefinition) {
        self.mappings.insert(name, definition);
    }

    /// Expand all `...TypeName` spreads in a JSONSelection
    ///
    /// This replaces `SpreadNamed` nodes with the corresponding mapping's selection.
    /// Handles recursive spreads and detects circular references.
    pub fn expand_selection(
        &self,
        selection: &JSONSelection,
    ) -> Result<JSONSelection, FederationError> {
        let mut expanding: HashSet<String> = HashSet::new();
        let no_subs = HashMap::new();
        let expanded_inner =
            self.expand_top_level(&selection.inner, &mut expanding, 0, &no_subs)?;

        Ok(JSONSelection {
            inner: expanded_inner,
            spec: selection.spec,
        })
    }

    /// Validate spread arguments against a mapping's parameters and build substitution map.
    fn build_substitutions(
        &self,
        mapping_name: &str,
        args: Option<&SpreadArgs>,
        parameters: &HashSet<String>,
    ) -> Result<HashMap<String, LitExpr>, FederationError> {
        let mut subs = HashMap::new();

        if parameters.is_empty() {
            // Mapping has no parameters; error if args are provided
            if let Some(args) = args
                && !args.args.is_empty()
            {
                return Err(FederationError::internal(format!(
                    "Spread `...{mapping_name}(...)` passes arguments, \
                     but mapping `{mapping_name}` has no parameters."
                )));
            }
            return Ok(subs);
        }

        let provided_args = args.map(|a| &a.args[..]).unwrap_or(&[]);

        // Check for duplicate arg names
        let mut seen: HashSet<&str> = HashSet::new();
        for arg in provided_args {
            let name = arg.name.as_ref().as_str();
            if !seen.insert(name) {
                return Err(FederationError::internal(format!(
                    "Spread `...{mapping_name}(...)` provides duplicate argument `{name}`."
                )));
            }
        }

        // Check for reserved name conflicts and unknown args; build subs map
        for arg in provided_args {
            let name = arg.name.as_ref().as_str();

            // Check if arg name conflicts with a runtime variable namespace
            if std::str::FromStr::from_str(&format!("${name}"))
                .is_ok_and(|_: Namespace| true)
            {
                return Err(FederationError::internal(format!(
                    "Spread argument name `{name}` conflicts with reserved \
                     runtime variable `${name}`."
                )));
            }

            if !parameters.contains(name) {
                let available: Vec<_> = parameters.iter().map(|s| s.as_str()).collect();
                return Err(FederationError::internal(format!(
                    "Spread `...{mapping_name}({name}: ...)` passes unknown argument `{name}`. \
                     Available parameters: {}",
                    available.join(", ")
                )));
            }

            // v1 restriction: spread arguments must be simple literals only.
            // Variables ($name), paths, objects, arrays, and expressions are not allowed.
            Self::validate_literal_value(mapping_name, name, arg.value.as_ref())?;

            // Store with the `$` prefix to match how variables appear in the AST
            subs.insert(format!("${name}"), arg.value.as_ref().clone());
        }

        // Check for missing required args (all params are required in v1)
        for param in parameters {
            if !subs.contains_key(&format!("${param}")) {
                return Err(FederationError::internal(format!(
                    "Spread `...{mapping_name}` is missing required argument `{param}`."
                )));
            }
        }

        Ok(subs)
    }

    /// Validate that a spread argument value is a simple literal (v1 restriction).
    /// Rejects variables ($name), paths, objects, arrays, and operator expressions.
    fn validate_literal_value(
        mapping_name: &str,
        arg_name: &str,
        value: &LitExpr,
    ) -> Result<(), FederationError> {
        match value {
            LitExpr::String(_) | LitExpr::Number(_) | LitExpr::Bool(_) | LitExpr::Null => Ok(()),
            LitExpr::Path(_) => Err(FederationError::internal(format!(
                "Spread `...{mapping_name}({arg_name}: ...)` uses a variable/path as argument value. \
                 In v1, spread arguments must be literals (integer, float, string, boolean, null). \
                 Inline the selection or use separate mappings."
            ))),
            LitExpr::Object(_) | LitExpr::Array(_) => Err(FederationError::internal(format!(
                "Spread `...{mapping_name}({arg_name}: ...)` uses a complex value as argument. \
                 In v1, spread arguments must be literals (integer, float, string, boolean, null)."
            ))),
            LitExpr::LitPath(_, _) | LitExpr::OpChain(_, _) => {
                Err(FederationError::internal(format!(
                    "Spread `...{mapping_name}({arg_name}: ...)` uses an expression as argument value. \
                     In v1, spread arguments must be literals (integer, float, string, boolean, null)."
                )))
            }
        }
    }

    /// Expand a TopLevelSelection
    fn expand_top_level(
        &self,
        top_level: &TopLevelSelection,
        expanding: &mut HashSet<String>,
        depth: usize,
        substitutions: &HashMap<String, LitExpr>,
    ) -> Result<TopLevelSelection, FederationError> {
        if depth > MAX_EXPANSION_DEPTH {
            return Err(FederationError::internal(format!(
                "Mapping expansion exceeded maximum depth of {}. \
                 This may indicate an overly complex mapping chain.",
                MAX_EXPANSION_DEPTH
            )));
        }

        match top_level {
            TopLevelSelection::Named(sub) => {
                let expanded =
                    self.expand_sub_selection(sub, expanding, depth, substitutions)?;
                if let [only] = expanded.selections.as_slice()
                    && (only.is_anonymous()
                        || matches!(only.prefix, NamingPrefix::Spread(None)))
                {
                    return Ok(TopLevelSelection::Path(only.path.clone()));
                }
                Ok(TopLevelSelection::Named(expanded))
            }
            TopLevelSelection::Path(path) => {
                let expanded =
                    self.expand_path_selection(path, expanding, depth, substitutions)?;
                Ok(TopLevelSelection::Path(expanded))
            }
        }
    }

    /// Expand a SubSelection, replacing any SpreadNamed nodes.
    fn expand_sub_selection(
        &self,
        sub: &SubSelection,
        expanding: &mut HashSet<String>,
        depth: usize,
        substitutions: &HashMap<String, LitExpr>,
    ) -> Result<SubSelection, FederationError> {
        let mut new_selections = Vec::new();

        for named in &sub.selections {
            match &named.prefix {
                NamingPrefix::SpreadNamed { name, args, .. } => {
                    let type_name = name.as_ref();

                    // Check for circular reference
                    if expanding.contains(type_name) {
                        return Err(FederationError::internal(format!(
                            "Circular reference detected in @mapping: ...{} references itself",
                            type_name
                        )));
                    }

                    // Look up the mapping
                    if let Some(mapping) = self.get_mapping(type_name) {
                        // Build substitution map from spread args
                        let subs = self.build_substitutions(
                            type_name,
                            args.as_ref(),
                            &mapping.parameters,
                        )?;

                        // Depth check before recursing into the referenced mapping.
                        let next_depth = depth + 1;
                        if next_depth > MAX_EXPANSION_DEPTH {
                            return Err(FederationError::internal(format!(
                                "Mapping expansion exceeded maximum depth of {}. \
                                 This may indicate an overly complex mapping chain.",
                                MAX_EXPANSION_DEPTH
                            )));
                        }

                        // Mark as expanding to detect cycles.
                        // Always remove after expansion (even on error) to avoid
                        // polluting the set for sibling expansions.
                        expanding.insert(type_name.to_string());

                        match &mapping.selection {
                            TopLevelSelection::Named(sub) => {
                                let result = self.expand_sub_selection(
                                    sub, expanding, next_depth, &subs,
                                );
                                expanding.remove(type_name);
                                new_selections.extend(result?.selections);
                            }
                            TopLevelSelection::Path(path) => {
                                let result = self.expand_path_selection(
                                    path, expanding, next_depth, &subs,
                                );
                                expanding.remove(type_name);
                                let expanded_path = result?;

                                // Anonymous paths with subselections behave like inline spreads.
                                let prefix = if expanded_path.is_anonymous()
                                    && expanded_path.has_subselection()
                                {
                                    NamingPrefix::Spread(None)
                                } else {
                                    NamingPrefix::None
                                };

                                new_selections.push(NamedSelection {
                                    prefix,
                                    path: expanded_path,
                                });
                            }
                        }
                    } else {
                        return Err(FederationError::internal(format!(
                            "Unknown mapping reference: ...{}. \
                             Make sure a @mapping directive is defined on type `{}`.",
                            type_name, type_name
                        )));
                    }
                }
                _ => {
                    // Recursively expand any nested selections
                    let expanded_named =
                        self.expand_named_selection(named, expanding, depth, substitutions)?;
                    new_selections.push(expanded_named);
                }
            }
        }

        if new_selections.len() > 1 && new_selections.iter().any(|s| s.is_anonymous()) {
            return Err(FederationError::internal(
                "Path selections cannot be combined with other selections at the same level. \
                 Use the path selection alone, or add a subselection like `$.path { ... }`."
                    .to_string(),
            ));
        }

        Ok(SubSelection {
            selections: new_selections,
            range: sub.range.clone(),
        })
    }

    /// Expand a NamedSelection, recursively expanding any nested selections
    fn expand_named_selection(
        &self,
        named: &NamedSelection,
        expanding: &mut HashSet<String>,
        depth: usize,
        substitutions: &HashMap<String, LitExpr>,
    ) -> Result<NamedSelection, FederationError> {
        let expanded_path =
            self.expand_path_selection(&named.path, expanding, depth, substitutions)?;

        Ok(NamedSelection {
            prefix: named.prefix.clone(),
            path: expanded_path,
        })
    }

    /// Expand a PathSelection, recursively expanding any nested SubSelections
    fn expand_path_selection(
        &self,
        path: &PathSelection,
        expanding: &mut HashSet<String>,
        depth: usize,
        substitutions: &HashMap<String, LitExpr>,
    ) -> Result<PathSelection, FederationError> {
        let expanded_path_list =
            self.expand_path_list(path.path.as_ref(), expanding, depth, substitutions)?;

        Ok(PathSelection {
            path: WithRange::new(expanded_path_list, path.path.range()),
        })
    }

    /// Expand a PathList, recursively expanding any nested SubSelections.
    /// Also performs parameter substitution when `substitutions` is non-empty.
    fn expand_path_list(
        &self,
        path_list: &PathList,
        expanding: &mut HashSet<String>,
        depth: usize,
        substitutions: &HashMap<String, LitExpr>,
    ) -> Result<PathList, FederationError> {
        match path_list {
            PathList::Selection(sub) => {
                let expanded =
                    self.expand_sub_selection(sub, expanding, depth, substitutions)?;
                Ok(PathList::Selection(expanded))
            }
            PathList::Key(key, tail) => {
                let expanded_tail =
                    self.expand_path_list(tail.as_ref(), expanding, depth, substitutions)?;
                Ok(PathList::Key(
                    key.clone(),
                    WithRange::new(expanded_tail, tail.range()),
                ))
            }
            PathList::Var(var, tail) => {
                // Check if this variable is a parameter that should be substituted
                if let KnownVariable::External(name) = var.as_ref()
                    && let Some(replacement) = substitutions.get(name.as_str())
                {
                    // Substitute: replace the variable with the literal value.
                    // The tail is expanded with substitutions in case there's more.
                    let expanded_tail = self.expand_path_list(
                        tail.as_ref(),
                        expanding,
                        depth,
                        substitutions,
                    )?;
                    return Ok(PathList::Expr(
                        WithRange::new(replacement.clone(), var.range()),
                        WithRange::new(expanded_tail, tail.range()),
                    ));
                }
                let expanded_tail =
                    self.expand_path_list(tail.as_ref(), expanding, depth, substitutions)?;
                Ok(PathList::Var(
                    var.clone(),
                    WithRange::new(expanded_tail, tail.range()),
                ))
            }
            PathList::Method(method, args, tail) => {
                // Expand method arguments (may contain parameter references)
                let expanded_args = match args {
                    Some(a) if !substitutions.is_empty() => {
                        let expanded = a
                            .args
                            .iter()
                            .map(|arg| {
                                let expanded_expr = self.expand_lit_expr(
                                    arg.as_ref(),
                                    expanding,
                                    depth,
                                    substitutions,
                                )?;
                                Ok(WithRange::new(expanded_expr, arg.range()))
                            })
                            .collect::<Result<Vec<_>, FederationError>>()?;
                        Some(MethodArgs {
                            args: expanded,
                            range: a.range.clone(),
                        })
                    }
                    other => other.clone(),
                };
                let expanded_tail =
                    self.expand_path_list(tail.as_ref(), expanding, depth, substitutions)?;
                Ok(PathList::Method(
                    method.clone(),
                    expanded_args,
                    WithRange::new(expanded_tail, tail.range()),
                ))
            }
            PathList::Expr(expr, tail) => {
                let expanded_expr =
                    self.expand_lit_expr(expr.as_ref(), expanding, depth, substitutions)?;
                let expanded_tail =
                    self.expand_path_list(tail.as_ref(), expanding, depth, substitutions)?;
                Ok(PathList::Expr(
                    WithRange::new(expanded_expr, expr.range()),
                    WithRange::new(expanded_tail, tail.range()),
                ))
            }
            PathList::Question(tail) => {
                let expanded_tail =
                    self.expand_path_list(tail.as_ref(), expanding, depth, substitutions)?;
                Ok(PathList::Question(WithRange::new(
                    expanded_tail,
                    tail.range(),
                )))
            }
            PathList::Empty => Ok(PathList::Empty),
        }
    }

    /// Expand path selections nested inside literal expressions.
    /// Also performs parameter substitution for `LitExpr::Path` nodes.
    fn expand_lit_expr(
        &self,
        lit_expr: &LitExpr,
        expanding: &mut HashSet<String>,
        depth: usize,
        substitutions: &HashMap<String, LitExpr>,
    ) -> Result<LitExpr, FederationError> {
        match lit_expr {
            LitExpr::String(_)
            | LitExpr::Number(_)
            | LitExpr::Bool(_)
            | LitExpr::Null => Ok(lit_expr.clone()),
            LitExpr::Object(obj) => {
                let mut expanded_obj = apollo_compiler::collections::IndexMap::default();
                for (key, value) in obj {
                    let expanded_value =
                        self.expand_lit_expr(value.as_ref(), expanding, depth, substitutions)?;
                    expanded_obj.insert(
                        key.clone(),
                        WithRange::new(expanded_value, value.range()),
                    );
                }
                Ok(LitExpr::Object(expanded_obj))
            }
            LitExpr::Array(arr) => {
                let mut expanded_arr = Vec::with_capacity(arr.len());
                for value in arr {
                    let expanded_value =
                        self.expand_lit_expr(value.as_ref(), expanding, depth, substitutions)?;
                    expanded_arr.push(WithRange::new(expanded_value, value.range()));
                }
                Ok(LitExpr::Array(expanded_arr))
            }
            LitExpr::Path(path) => {
                // Check if this is a bare parameter variable (e.g., $count with no
                // trailing path). If so, substitute directly as a LitExpr to avoid
                // wrapping in $(...) syntax.
                if let PathList::Var(var, tail) = path.path.as_ref()
                    && let KnownVariable::External(name) = var.as_ref()
                    && matches!(tail.as_ref(), PathList::Empty)
                    && let Some(replacement) = substitutions.get(name.as_str())
                {
                    return Ok(replacement.clone());
                }
                let expanded_path =
                    self.expand_path_selection(path, expanding, depth, substitutions)?;
                Ok(LitExpr::Path(expanded_path))
            }
            LitExpr::LitPath(literal, subpath) => {
                let expanded_literal =
                    self.expand_lit_expr(literal.as_ref(), expanding, depth, substitutions)?;
                let expanded_subpath =
                    self.expand_path_list(subpath.as_ref(), expanding, depth, substitutions)?;
                Ok(LitExpr::LitPath(
                    WithRange::new(expanded_literal, literal.range()),
                    WithRange::new(expanded_subpath, subpath.range()),
                ))
            }
            LitExpr::OpChain(op, operands) => {
                let mut expanded_operands = Vec::with_capacity(operands.len());
                for operand in operands {
                    let expanded_operand =
                        self.expand_lit_expr(operand.as_ref(), expanding, depth, substitutions)?;
                    expanded_operands.push(WithRange::new(expanded_operand, operand.range()));
                }
                Ok(LitExpr::OpChain(op.clone(), expanded_operands))
            }
        }
    }

    /// Check if the registry is empty
    pub fn is_empty(&self) -> bool {
        self.mappings.is_empty()
    }

    /// Get the number of mappings in the registry
    pub fn len(&self) -> usize {
        self.mappings.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::json_selection::PrettyPrintable;

    #[test]
    fn test_auto_map_selection_generation() {
        use apollo_compiler::name;

        let field_names = vec![name!(id), name!(name), name!(email)];
        let selection =
            MappingRegistry::generate_auto_map_selection(&name!(TestType), &field_names, ConnectSpec::V0_5).unwrap();

        match selection {
            TopLevelSelection::Named(sub) => assert_eq!(sub.selections.len(), 3),
            TopLevelSelection::Path(_) => panic!("auto-map should generate named selection"),
        }
    }

    #[test]
    fn test_methods_allowed_in_mapping_selection() {
        use apollo_compiler::name;

        // Methods ARE now allowed in @mapping selections
        let args = MappingDirectiveArguments {
            type_name: name!(User),
            alias: name!(User),
            selection: Some("id name: fullName->echo".to_string()),
            field_names: vec![name!(id), name!(name)],
        };

        let result = MappingRegistry::build_mapping_definition(&args, ConnectSpec::V0_5);
        assert!(
            result.is_ok(),
            "Methods should be allowed in @mapping selections"
        );

        // Verify the selection was parsed correctly
        let definition = result.unwrap();
        match definition.selection {
            TopLevelSelection::Named(sub) => assert_eq!(sub.selections.len(), 2),
            TopLevelSelection::Path(_) => panic!("expected named selection"),
        }
    }

    #[test]
    fn test_spread_named_with_method_is_separate_token() {
        // Verify that ...TypeName->method() parses as TWO separate things:
        // 1. SpreadNamed "User"
        // 2. Unparseable remainder "->method()"
        //
        // The parser creates SpreadNamed with path: PathSelection::empty(),
        // so ->method() cannot attach to it. It becomes the remainder which
        // fails to parse as a valid next selection.

        let input = "...User->first()";
        let result = JSONSelection::parse_with_spec(input, ConnectSpec::V0_5);

        // This should fail because "->first()" is not a valid selection start
        assert!(
            result.is_err(),
            "...Type->method() should fail to parse: {:?}",
            result
        );
    }

    #[test]
    fn test_expand_simple_spread() {
        use apollo_compiler::name;

        // Create a registry with a User mapping
        let mut registry = MappingRegistry::new();
        let user_selection =
            JSONSelection::parse_with_spec("id name email", ConnectSpec::V0_5).unwrap();
        let TopLevelSelection::Named(sub) = user_selection.inner else { panic!("expected Named selection") };
        registry.mappings.insert(
            name!(User),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(User),
                parameters: HashSet::new(),
            },
        );

        // Parse a selection with ...User
        let selection = JSONSelection::parse_with_spec("...User", ConnectSpec::V0_5).unwrap();

        // Expand the selection
        let expanded = registry.expand_selection(&selection).unwrap();

        // Verify the expansion
        assert_eq!(expanded.pretty_print(), "id\nname\nemail");
    }

    #[test]
    fn test_expand_spread_with_additional_fields() {
        use apollo_compiler::name;

        // Create a registry with a User mapping
        let mut registry = MappingRegistry::new();
        let user_selection = JSONSelection::parse_with_spec("id name", ConnectSpec::V0_5).unwrap();
        let TopLevelSelection::Named(sub) = user_selection.inner else { panic!("expected Named selection") };
        registry.mappings.insert(
            name!(User),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(User),
                parameters: HashSet::new(),
            },
        );

        // Parse a selection with ...User and extra field
        let selection =
            JSONSelection::parse_with_spec("...User extraField", ConnectSpec::V0_5).unwrap();

        // Expand the selection
        let expanded = registry.expand_selection(&selection).unwrap();

        // Verify the expansion includes both the User fields and the extra field
        let pretty = expanded.pretty_print();
        assert!(pretty.contains("id"));
        assert!(pretty.contains("name"));
        assert!(pretty.contains("extraField"));
    }

    #[test]
    fn test_circular_reference_detection() {
        use apollo_compiler::name;

        // Create a registry with circular references
        let mut registry = MappingRegistry::new();

        // UserA references UserB
        let user_a_selection =
            JSONSelection::parse_with_spec("id ...UserB", ConnectSpec::V0_5).unwrap();
        let TopLevelSelection::Named(sub) = user_a_selection.inner else { panic!("expected Named selection") };
        registry.mappings.insert(
            name!(UserA),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(UserA),
                parameters: HashSet::new(),
            },
        );

        // UserB references UserA (circular!)
        let user_b_selection =
            JSONSelection::parse_with_spec("name ...UserA", ConnectSpec::V0_5).unwrap();
        let TopLevelSelection::Named(sub) = user_b_selection.inner else { panic!("expected Named selection") };
        registry.mappings.insert(
            name!(UserB),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(UserB),
                parameters: HashSet::new(),
            },
        );

        // Try to expand UserA - should fail with circular reference error
        let selection = JSONSelection::parse_with_spec("...UserA", ConnectSpec::V0_5).unwrap();
        let result = registry.expand_selection(&selection);

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Circular reference"));
    }

    #[test]
    fn test_circular_reference_detection_with_path_mappings() {
        use apollo_compiler::name;

        let mut registry = MappingRegistry::new();

        let map_a_selection =
            JSONSelection::parse_with_spec("$.root { ...MapB }", ConnectSpec::V0_5).unwrap();
        registry.mappings.insert(
            name!(MapA),
            MappingDefinition {
                selection: map_a_selection.inner,
                source_type: name!(TypeA),
                parameters: HashSet::new(),
            },
        );

        let map_b_selection =
            JSONSelection::parse_with_spec("$.other { ...MapA }", ConnectSpec::V0_5).unwrap();
        registry.mappings.insert(
            name!(MapB),
            MappingDefinition {
                selection: map_b_selection.inner,
                source_type: name!(TypeB),
                parameters: HashSet::new(),
            },
        );

        let selection = JSONSelection::parse_with_spec("...MapA", ConnectSpec::V0_5).unwrap();
        let result = registry.expand_selection(&selection);

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Circular reference"));
    }

    #[test]
    fn test_unknown_mapping_error() {
        let registry = MappingRegistry::new();

        // Try to expand a spread that references a non-existent mapping
        let selection =
            JSONSelection::parse_with_spec("...UnknownType", ConnectSpec::V0_5).unwrap();
        let result = registry.expand_selection(&selection);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown mapping"));
    }

    #[test]
    fn test_nested_spread_expansion() {
        use apollo_compiler::name;

        // Create a registry with nested mappings
        let mut registry = MappingRegistry::new();

        // Address mapping
        let address_selection =
            JSONSelection::parse_with_spec("street city zipCode", ConnectSpec::V0_5).unwrap();
        let TopLevelSelection::Named(sub) = address_selection.inner else { panic!("expected Named selection") };
        registry.mappings.insert(
            name!(Address),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(Address),
                parameters: HashSet::new(),
            },
        );

        // User mapping references Address
        let user_selection =
            JSONSelection::parse_with_spec("id name address { ...Address }", ConnectSpec::V0_5)
                .unwrap();
        let TopLevelSelection::Named(sub) = user_selection.inner else { panic!("expected Named selection") };
        registry.mappings.insert(
            name!(User),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(User),
                parameters: HashSet::new(),
            },
        );

        // Expand User
        let selection = JSONSelection::parse_with_spec("...User", ConnectSpec::V0_5).unwrap();
        let expanded = registry.expand_selection(&selection).unwrap();

        let pretty = expanded.pretty_print();
        assert!(pretty.contains("id"));
        assert!(pretty.contains("name"));
        assert!(pretty.contains("address"));
        assert!(pretty.contains("street"));
        assert!(pretty.contains("city"));
        assert!(pretty.contains("zipCode"));
    }

    #[test]
    fn test_spread_with_alias() {
        use apollo_compiler::name;

        // Create a registry with a mapping using alias
        let mut registry = MappingRegistry::new();

        // Create User mapping aliased as "BasicUser"
        let user_selection = JSONSelection::parse_with_spec("id name", ConnectSpec::V0_5).unwrap();
        let TopLevelSelection::Named(sub) = user_selection.inner else { panic!("expected Named selection") };
        registry.mappings.insert(
            name!(BasicUser),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(User),
                parameters: HashSet::new(),
            },
        );

        // Expand using alias
        let selection = JSONSelection::parse_with_spec("...BasicUser", ConnectSpec::V0_5).unwrap();
        let expanded = registry.expand_selection(&selection).unwrap();

        let pretty = expanded.pretty_print();
        assert!(pretty.contains("id"));
        assert!(pretty.contains("name"));
    }

    #[test]
    fn test_spread_preserves_other_selections() {
        use apollo_compiler::name;

        // Create a registry with a User mapping
        let mut registry = MappingRegistry::new();
        let user_selection = JSONSelection::parse_with_spec("id name", ConnectSpec::V0_5).unwrap();
        let TopLevelSelection::Named(sub) = user_selection.inner else { panic!("expected Named selection") };
        registry.mappings.insert(
            name!(User),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(User),
                parameters: HashSet::new(),
            },
        );

        // Parse a selection with spread + other fields + nested selection
        let selection = JSONSelection::parse_with_spec(
            "...User email posts { title content }",
            ConnectSpec::V0_5,
        )
        .unwrap();

        // Expand the selection
        let expanded = registry.expand_selection(&selection).unwrap();

        let pretty = expanded.pretty_print();
        // Spread fields
        assert!(pretty.contains("id"));
        assert!(pretty.contains("name"));
        // Additional field
        assert!(pretty.contains("email"));
        // Nested selection
        assert!(pretty.contains("posts"));
        assert!(pretty.contains("title"));
        assert!(pretty.contains("content"));
    }

    #[test]
    fn test_expand_path_selection_with_spread() {
        use apollo_compiler::name;

        // Create a registry with a mapping
        let mut registry = MappingRegistry::new();
        let user_selection = JSONSelection::parse_with_spec("id name", ConnectSpec::V0_5).unwrap();
        let TopLevelSelection::Named(sub) = user_selection.inner else { panic!("expected Named selection") };
        registry.mappings.insert(
            name!(User),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(User),
                parameters: HashSet::new(),
            },
        );

        // Parse a selection with path containing a nested subselection with spread
        let selection =
            JSONSelection::parse_with_spec("users: $.data.users { ...User }", ConnectSpec::V0_5)
                .unwrap();

        // Expand the selection
        let expanded = registry.expand_selection(&selection).unwrap();

        let pretty = expanded.pretty_print();
        assert!(pretty.contains("users:"));
        assert!(pretty.contains("id"));
        assert!(pretty.contains("name"));
    }

    #[test]
    fn test_multiple_spreads_same_selection() {
        use apollo_compiler::name;

        // Create a registry with multiple mappings
        let mut registry = MappingRegistry::new();

        let user_selection = JSONSelection::parse_with_spec("id name", ConnectSpec::V0_5).unwrap();
        let TopLevelSelection::Named(sub) = user_selection.inner else { panic!("expected Named selection") };
        registry.mappings.insert(
            name!(UserBasic),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(User),
                parameters: HashSet::new(),
            },
        );

        let contact_selection =
            JSONSelection::parse_with_spec("email phone", ConnectSpec::V0_5).unwrap();
        let TopLevelSelection::Named(sub) = contact_selection.inner else { panic!("expected Named selection") };
        registry.mappings.insert(
            name!(ContactInfo),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(User),
                parameters: HashSet::new(),
            },
        );

        // Parse a selection with multiple spreads
        let selection =
            JSONSelection::parse_with_spec("...UserBasic ...ContactInfo", ConnectSpec::V0_5)
                .unwrap();

        // Expand the selection
        let expanded = registry.expand_selection(&selection).unwrap();

        let pretty = expanded.pretty_print();
        assert!(pretty.contains("id"));
        assert!(pretty.contains("name"));
        assert!(pretty.contains("email"));
        assert!(pretty.contains("phone"));
    }

    #[test]
    fn test_from_schema_integration() {
        use apollo_compiler::Schema;

        let schema = Schema::parse(
            r#"
            extend schema @link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@mapping"])
            directive @link(url: String, import: [link__Import]) repeatable on SCHEMA
            scalar link__Import
            directive @mapping(selection: String, as: String) repeatable on OBJECT | INTERFACE

            type User @mapping {
                id: ID!
                name: String!
                email: String!
            }

            type Product @mapping(selection: "sku: product_sku title: product_title", as: "ProductV2") {
                sku: ID!
                title: String!
            }

            type Query {
                users: [User]
                products: [Product]
            }
            "#,
            "test.graphql",
        )
        .unwrap();

        let registry = MappingRegistry::from_schema(&schema).unwrap();

        // Auto-mapped User should be in registry
        assert!(registry.has_mapping("User"));

        // Aliased mapping should be in registry
        assert!(registry.has_mapping("ProductV2"));

        // Original Product type should NOT have a separate mapping (only ProductV2)
        assert!(!registry.has_mapping("Product"));
    }

    #[test]
    fn test_from_schema_integration_with_path_alias() {
        use apollo_compiler::Schema;

        let schema = Schema::parse(
            r#"
            extend schema @link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@mapping"])
            directive @link(url: String, import: [link__Import]) repeatable on SCHEMA
            scalar link__Import
            directive @mapping(selection: String, as: String) repeatable on OBJECT | INTERFACE

            type Data @mapping(selection: "$.response.data", as: "DataPath") {
                id: ID!
            }

            type Query {
                data: Data
            }
            "#,
            "test.graphql",
        )
        .unwrap();

        let registry = MappingRegistry::from_schema(&schema).unwrap();

        assert!(registry.has_mapping("DataPath"));
        assert!(!registry.has_mapping("Data"));

        let mapping = registry.get_mapping("DataPath").unwrap();
        assert!(matches!(mapping.selection, TopLevelSelection::Path(_)));
    }

    #[test]
    fn test_auto_map_empty_fields_error() {
        use apollo_compiler::name;
        // Auto-map with no fields should fail
        let result = MappingRegistry::generate_auto_map_selection(&name!(EmptyType), &[], ConnectSpec::V0_5);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no fields"));
    }

    #[test]
    fn test_invalid_selection_syntax_error() {
        use apollo_compiler::name;

        let args = MappingDirectiveArguments {
            type_name: name!(User),
            alias: name!(User),
            selection: Some("{ invalid [ syntax".to_string()),
            field_names: vec![name!(id)],
        };

        let result = MappingRegistry::build_mapping_definition(&args, ConnectSpec::V0_5);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Failed to parse"));
    }

    #[test]
    fn test_path_selection_allowed_in_mapping() {
        use apollo_compiler::name;

        // Path selection (starting with $) is allowed in @mapping
        let args = MappingDirectiveArguments {
            type_name: name!(User),
            alias: name!(User),
            selection: Some("$.data.id".to_string()),
            field_names: vec![name!(id)],
        };

        let result = MappingRegistry::build_mapping_definition(&args, ConnectSpec::V0_5);
        assert!(result.is_ok());
    }

    #[test]
    fn test_path_mapping_top_level_parity() {
        use apollo_compiler::name;

        let mut registry = MappingRegistry::new();
        let mapping_selection =
            JSONSelection::parse_with_spec("$.data.user", ConnectSpec::V0_5).unwrap();
        registry.mappings.insert(
            name!(UserPath),
            MappingDefinition {
                selection: mapping_selection.inner,
                source_type: name!(User),
                parameters: HashSet::new(),
            },
        );

        let selection = JSONSelection::parse_with_spec("...UserPath", ConnectSpec::V0_5).unwrap();
        let expanded = registry.expand_selection(&selection).unwrap();

        let inline = JSONSelection::parse_with_spec("$.data.user", ConnectSpec::V0_5).unwrap();
        assert_eq!(expanded.pretty_print(), inline.pretty_print());
    }

    #[test]
    fn test_path_mapping_with_method_parity() {
        use apollo_compiler::name;

        let mut registry = MappingRegistry::new();
        let mapping_selection =
            JSONSelection::parse_with_spec("$.data.users->first { id }", ConnectSpec::V0_5)
                .unwrap();
        registry.mappings.insert(
            name!(UserPath),
            MappingDefinition {
                selection: mapping_selection.inner,
                source_type: name!(User),
                parameters: HashSet::new(),
            },
        );

        let selection = JSONSelection::parse_with_spec("...UserPath", ConnectSpec::V0_5).unwrap();
        let expanded = registry.expand_selection(&selection).unwrap();

        let inline =
            JSONSelection::parse_with_spec("$.data.users->first { id }", ConnectSpec::V0_5)
                .unwrap();
        assert_eq!(expanded.pretty_print(), inline.pretty_print());
    }

    #[test]
    fn test_path_mapping_with_subselection_allows_siblings() {
        use apollo_compiler::name;

        let mut registry = MappingRegistry::new();
        let mapping_selection =
            JSONSelection::parse_with_spec("$.data.user { id }", ConnectSpec::V0_5).unwrap();
        registry.mappings.insert(
            name!(UserPath),
            MappingDefinition {
                selection: mapping_selection.inner,
                source_type: name!(User),
                parameters: HashSet::new(),
            },
        );

        let selection =
            JSONSelection::parse_with_spec("...UserPath name", ConnectSpec::V0_5).unwrap();
        let expanded = registry.expand_selection(&selection).unwrap();

        let inline =
            JSONSelection::parse_with_spec("$.data.user { id } name", ConnectSpec::V0_5).unwrap();
        assert_eq!(expanded.pretty_print(), inline.pretty_print());
    }

    #[test]
    fn test_path_mapping_with_sibling_selections_errors() {
        use apollo_compiler::name;

        let mut registry = MappingRegistry::new();
        let mapping_selection =
            JSONSelection::parse_with_spec("$.data.user", ConnectSpec::V0_5).unwrap();
        registry.mappings.insert(
            name!(UserPath),
            MappingDefinition {
                selection: mapping_selection.inner,
                source_type: name!(User),
                parameters: HashSet::new(),
            },
        );

        let selection =
            JSONSelection::parse_with_spec("...UserPath name", ConnectSpec::V0_5).unwrap();
        let err = registry.expand_selection(&selection).unwrap_err().to_string();
        assert!(
            err.contains("Path selections cannot be combined with other selections at the same level")
        );
    }

    #[test]
    fn test_path_mapping_nested_path_mapping_parity() {
        use apollo_compiler::name;

        let mut registry = MappingRegistry::new();
        let inner_selection =
            JSONSelection::parse_with_spec("$.inner { id }", ConnectSpec::V0_5).unwrap();
        registry.mappings.insert(
            name!(InnerPath),
            MappingDefinition {
                selection: inner_selection.inner,
                source_type: name!(Inner),
                parameters: HashSet::new(),
            },
        );

        let outer_selection =
            JSONSelection::parse_with_spec("$.outer { ...InnerPath }", ConnectSpec::V0_5).unwrap();
        registry.mappings.insert(
            name!(OuterPath),
            MappingDefinition {
                selection: outer_selection.inner,
                source_type: name!(Outer),
                parameters: HashSet::new(),
            },
        );

        let selection = JSONSelection::parse_with_spec("...OuterPath", ConnectSpec::V0_5).unwrap();
        let expanded = registry.expand_selection(&selection).unwrap();

        let inline =
            JSONSelection::parse_with_spec("$.outer { $.inner { id } }", ConnectSpec::V0_5)
                .unwrap();
        assert_eq!(expanded.pretty_print(), inline.pretty_print());
    }

    #[test]
    fn test_duplicate_alias_last_wins() {
        use apollo_compiler::name;

        // When two mappings have the same alias, the last one wins (IndexMap behavior)
        let mut registry = MappingRegistry::new();

        let selection1 = JSONSelection::parse_with_spec("id", ConnectSpec::V0_5).unwrap();
        let TopLevelSelection::Named(sub) = selection1.inner else { panic!("expected Named selection") };
        registry.mappings.insert(
            name!(UserMapping),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(User),
                parameters: HashSet::new(),
            },
        );

        let selection2 =
            JSONSelection::parse_with_spec("id name email", ConnectSpec::V0_5).unwrap();
        let TopLevelSelection::Named(sub) = selection2.inner else { panic!("expected Named selection") };
        registry.mappings.insert(
            name!(UserMapping),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(Admin),
                parameters: HashSet::new(),
            },
        );

        assert_eq!(registry.len(), 1);
        let mapping = registry.get_mapping("UserMapping").unwrap();
        assert_eq!(mapping.source_type, name!(Admin));
    }

    #[test]
    fn test_both_spread_types_together() {
        // Test that both anonymous spread (...path) and named spread (...TypeName) work
        // Named spread (uppercase) references a @mapping
        let named_spread = JSONSelection::parse_with_spec("...User", ConnectSpec::V0_5).unwrap();
        let named_pretty = named_spread.pretty_print();
        assert!(
            named_pretty.contains("...User"),
            "Named spread ...User not found in: {}",
            named_pretty
        );

        // Anonymous spread (lowercase path) spreads a path into the result
        // Syntax is `...path` or `...path { subfields }`
        // Note: pretty print outputs `... metadata` (with space after `...`)
        let anon_spread =
            JSONSelection::parse_with_spec("...metadata { id name }", ConnectSpec::V0_5).unwrap();
        let anon_pretty = anon_spread.pretty_print();
        assert!(
            anon_pretty.contains("... metadata"),
            "Anonymous spread ... metadata not found in: {}",
            anon_pretty
        );
    }

    // =========================================================================
    // Regression tests for review finding C2.
    // =========================================================================

    // C2: PathList::Expr must recurse into nested LitExpr::Path values so
    // SpreadNamed nodes are expanded inside `$(...)` expressions.

    #[test]
    fn expand_spread_in_nested_subselection() {
        use apollo_compiler::name;

        // Create registry with mappings
        let mut registry = MappingRegistry::new();

        let user_selection =
            JSONSelection::parse_with_spec("id name", ConnectSpec::V0_5).unwrap();
        let TopLevelSelection::Named(sub) = user_selection.inner else { panic!("expected Named selection") };
        registry.mappings.insert(
            name!(User),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(User),
                parameters: HashSet::new(),
            },
        );

        let addr_selection =
            JSONSelection::parse_with_spec("street city", ConnectSpec::V0_5).unwrap();
        let TopLevelSelection::Named(sub) = addr_selection.inner else { panic!("expected Named selection") };
        registry.mappings.insert(
            name!(Address),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(Address),
                parameters: HashSet::new(),
            },
        );

        // Selection with spreads inside a path subselection:
        // users: $.data { ...User address { ...Address } }
        let selection = JSONSelection::parse_with_spec(
            "users: $.data { ...User address { ...Address } }",
            ConnectSpec::V0_5,
        )
        .unwrap();

        let expanded = registry.expand_selection(&selection).unwrap();
        let pretty = expanded.pretty_print();

        // Both spreads should be fully expanded
        assert!(
            !pretty.contains("...User"),
            "...User should be expanded. Got: {}",
            pretty
        );
        assert!(
            !pretty.contains("...Address"),
            "...Address should be expanded. Got: {}",
            pretty
        );
        assert!(pretty.contains("id"), "Expected 'id' in: {}", pretty);
        assert!(pretty.contains("name"), "Expected 'name' in: {}", pretty);
        assert!(
            pretty.contains("street"),
            "Expected 'street' in: {}",
            pretty
        );
        assert!(pretty.contains("city"), "Expected 'city' in: {}", pretty);
    }

    #[test]
    fn expand_spread_inside_path_expression_literal() {
        use apollo_compiler::name;

        let mut registry = MappingRegistry::new();

        let user_selection =
            JSONSelection::parse_with_spec("id name", ConnectSpec::V0_5).unwrap();
        let TopLevelSelection::Named(sub) = user_selection.inner else { panic!("expected Named selection") };
        registry.mappings.insert(
            name!(User),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(User),
                parameters: HashSet::new(),
            },
        );

        let selection =
            JSONSelection::parse_with_spec("payload: $(data { ...User })", ConnectSpec::V0_5)
                .unwrap();
        let expanded = registry.expand_selection(&selection).unwrap();

        let inline =
            JSONSelection::parse_with_spec("payload: $(data { id name })", ConnectSpec::V0_5)
                .unwrap();
        assert_eq!(expanded.pretty_print(), inline.pretty_print());
    }

    #[test]
    fn expand_three_levels_of_transitive_spreads() {
        use apollo_compiler::name;

        // Create a 3-level deep chain: Order -> ...User -> address { ...Address }
        let mut registry = MappingRegistry::new();

        let addr_selection =
            JSONSelection::parse_with_spec("street city zip", ConnectSpec::V0_5).unwrap();
        let TopLevelSelection::Named(sub) = addr_selection.inner else { panic!("expected Named selection") };
        registry.mappings.insert(
            name!(Address),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(Address),
                parameters: HashSet::new(),
            },
        );

        // User references Address
        let user_selection = JSONSelection::parse_with_spec(
            "id name address { ...Address }",
            ConnectSpec::V0_5,
        )
        .unwrap();
        let TopLevelSelection::Named(sub) = user_selection.inner else { panic!("expected Named selection") };
        registry.mappings.insert(
            name!(User),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(User),
                parameters: HashSet::new(),
            },
        );

        // Top-level references User
        let selection = JSONSelection::parse_with_spec(
            "...User email",
            ConnectSpec::V0_5,
        )
        .unwrap();

        let expanded = registry.expand_selection(&selection).unwrap();
        let pretty = expanded.pretty_print();

        // All three levels should be fully expanded
        assert!(
            !pretty.contains("...User"),
            "...User should be expanded. Got: {}",
            pretty
        );
        assert!(
            !pretty.contains("...Address"),
            "...Address should be expanded. Got: {}",
            pretty
        );

        // Verify all leaf fields are present
        for field in &["id", "name", "street", "city", "zip", "email"] {
            assert!(pretty.contains(field), "Expected '{}' in: {}", field, pretty);
        }
    }

    #[test]
    fn expand_rejects_chain_exceeding_max_depth() {
        // C2 FIX: The MAX_EXPANSION_DEPTH check is now in expand_sub_selection,
        // so deep non-circular chains are caught.
        let mut registry = MappingRegistry::new();

        for i in 0..35 {
            let this_name = Name::new(&format!("T{i}")).unwrap();
            let next_ref = if i < 34 {
                format!("field{i} ...T{}", i + 1)
            } else {
                format!("field{i}")
            };

            let sel = JSONSelection::parse_with_spec(&next_ref, ConnectSpec::V0_5).unwrap();
            let TopLevelSelection::Named(sub) = sel.inner else { panic!("expected Named selection") };
            registry.mappings.insert(
                this_name.clone(),
                MappingDefinition {
                    selection: TopLevelSelection::Named(sub),
                    source_type: this_name,
                    parameters: HashSet::new(),
                },
            );
        }

        // Try to expand T0 -- this creates a 35-level chain which exceeds MAX_EXPANSION_DEPTH (32).
        let selection = JSONSelection::parse_with_spec("...T0", ConnectSpec::V0_5).unwrap();
        let result = registry.expand_selection(&selection);

        assert!(
            result.is_err(),
            "35-level chain should be rejected by depth check"
        );
        assert!(
            result.unwrap_err().to_string().contains("maximum depth"),
            "Error should mention maximum depth"
        );
    }

    /// Regression test: cycle detection set must be cleaned up even when
    /// expansion fails. Previously, an error in one branch would leave the
    /// expanding set polluted, causing false cycle-detection errors in
    /// sibling expansions.
    #[test]
    fn test_cycle_detection_cleanup_on_error() {
        use apollo_compiler::name;

        let mut registry = MappingRegistry::new();

        // A mapping that references an unknown mapping (will error)
        let bad_selection =
            JSONSelection::parse_with_spec("...Unknown", ConnectSpec::V0_5).unwrap();
        let TopLevelSelection::Named(bad_sub) = bad_selection.inner else { panic!("expected Named selection") };

        registry.mappings.insert(
            name!(Bad),
            MappingDefinition {
                selection: TopLevelSelection::Named(bad_sub),
                source_type: name!(Bad),
                parameters: HashSet::new(),
            },
        );

        // A good mapping
        let good_selection =
            JSONSelection::parse_with_spec("id name", ConnectSpec::V0_5).unwrap();
        registry.mappings.insert(
            name!(Good),
            MappingDefinition {
                selection: good_selection.inner,
                source_type: name!(Good),
                parameters: HashSet::new(),
            },
        );

        // Expanding ...Bad should fail (references ...Unknown)
        let sel_bad = JSONSelection::parse_with_spec("...Bad", ConnectSpec::V0_5).unwrap();
        assert!(registry.expand_selection(&sel_bad).is_err());

        // Expanding ...Good should still succeed (expanding set must be clean)
        let sel_good = JSONSelection::parse_with_spec("...Good", ConnectSpec::V0_5).unwrap();
        assert!(
            registry.expand_selection(&sel_good).is_ok(),
            "Expanding ...Good should succeed after ...Bad failed"
        );
    }

    // =========================================================================
    // @mapping arguments (parameterized spreads) tests
    // =========================================================================

    #[test]
    fn test_basic_parameter_substitution() {
        use apollo_compiler::name;

        let mut registry = MappingRegistry::new();

        // Mapping with a parameter: friends->slice(0, $count)
        let mapping_selection = JSONSelection::parse_with_spec(
            "id name friends: friends->slice(0, $count)",
            ConnectSpec::V0_5,
        )
        .unwrap();
        let TopLevelSelection::Named(sub) = mapping_selection.inner else {
            panic!("expected Named selection")
        };
        let mut params = HashSet::new();
        params.insert("count".to_string());
        registry.mappings.insert(
            name!(User),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(User),
                parameters: params,
            },
        );

        // Spread with argument: ...User(count: 5)
        let selection =
            JSONSelection::parse_with_spec("...User(count: 5)", ConnectSpec::V0_5).unwrap();
        let expanded = registry.expand_selection(&selection).unwrap();

        let pretty = expanded.pretty_print();
        assert!(
            pretty.contains("friends: friends->slice(0, 5)"),
            "Expected substitution of $count with 5, got: {}",
            pretty
        );
    }

    #[test]
    fn test_multiple_parameters() {
        use apollo_compiler::name;

        let mut registry = MappingRegistry::new();

        let mapping_selection = JSONSelection::parse_with_spec(
            "items: items->slice($offset, $limit)",
            ConnectSpec::V0_5,
        )
        .unwrap();
        let TopLevelSelection::Named(sub) = mapping_selection.inner else {
            panic!("expected Named selection")
        };
        let mut params = HashSet::new();
        params.insert("offset".to_string());
        params.insert("limit".to_string());
        registry.mappings.insert(
            name!(Paginated),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(Paginated),
                parameters: params,
            },
        );

        let selection = JSONSelection::parse_with_spec(
            "...Paginated(offset: 0, limit: 10)",
            ConnectSpec::V0_5,
        )
        .unwrap();
        let expanded = registry.expand_selection(&selection).unwrap();

        let pretty = expanded.pretty_print();
        assert!(
            pretty.contains("items: items->slice(0, 10)"),
            "Expected substitution of both params, got: {}",
            pretty
        );
    }

    #[test]
    fn test_missing_required_argument_error() {
        use apollo_compiler::name;

        let mut registry = MappingRegistry::new();

        let mapping_selection = JSONSelection::parse_with_spec(
            "friends: friends->slice(0, $count)",
            ConnectSpec::V0_5,
        )
        .unwrap();
        let TopLevelSelection::Named(sub) = mapping_selection.inner else {
            panic!("expected Named selection")
        };
        let mut params = HashSet::new();
        params.insert("count".to_string());
        registry.mappings.insert(
            name!(User),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(User),
                parameters: params,
            },
        );

        // Spread without required argument
        let selection =
            JSONSelection::parse_with_spec("...User", ConnectSpec::V0_5).unwrap();
        let result = registry.expand_selection(&selection);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("missing required argument"),
            "Expected missing arg error, got: {}",
            err
        );
    }

    #[test]
    fn test_unknown_argument_error() {
        use apollo_compiler::name;

        let mut registry = MappingRegistry::new();

        let mapping_selection =
            JSONSelection::parse_with_spec("id name", ConnectSpec::V0_5).unwrap();
        let TopLevelSelection::Named(sub) = mapping_selection.inner else {
            panic!("expected Named selection")
        };
        registry.mappings.insert(
            name!(User),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(User),
                parameters: HashSet::new(),
            },
        );

        // Spread passes arguments on a parameterless mapping
        let selection =
            JSONSelection::parse_with_spec("...User(count: 5)", ConnectSpec::V0_5).unwrap();
        let result = registry.expand_selection(&selection);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("has no parameters"),
            "Expected no-params error, got: {}",
            err
        );
    }

    #[test]
    fn test_duplicate_argument_error() {
        use apollo_compiler::name;

        let mut registry = MappingRegistry::new();

        let mapping_selection = JSONSelection::parse_with_spec(
            "friends: friends->slice(0, $count)",
            ConnectSpec::V0_5,
        )
        .unwrap();
        let TopLevelSelection::Named(sub) = mapping_selection.inner else {
            panic!("expected Named selection")
        };
        let mut params = HashSet::new();
        params.insert("count".to_string());
        registry.mappings.insert(
            name!(User),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(User),
                parameters: params,
            },
        );

        // Spread with duplicate argument
        let selection = JSONSelection::parse_with_spec(
            "...User(count: 5, count: 10)",
            ConnectSpec::V0_5,
        )
        .unwrap();
        let result = registry.expand_selection(&selection);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("duplicate argument"),
            "Expected duplicate arg error, got: {}",
            err
        );
    }

    #[test]
    fn test_runtime_variables_pass_through() {
        use apollo_compiler::name;

        let mut registry = MappingRegistry::new();

        // Mapping with both runtime vars ($this) and parameters ($count)
        let mapping_selection = JSONSelection::parse_with_spec(
            "name: $this.displayName friends: friends->slice(0, $count)",
            ConnectSpec::V0_5,
        )
        .unwrap();
        let TopLevelSelection::Named(sub) = mapping_selection.inner else {
            panic!("expected Named selection")
        };
        let mut params = HashSet::new();
        params.insert("count".to_string());
        registry.mappings.insert(
            name!(User),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(User),
                parameters: params,
            },
        );

        let selection =
            JSONSelection::parse_with_spec("...User(count: 5)", ConnectSpec::V0_5).unwrap();
        let expanded = registry.expand_selection(&selection).unwrap();

        let pretty = expanded.pretty_print();
        // $this should remain untouched
        assert!(
            pretty.contains("$this"),
            "Expected $this to pass through, got: {}",
            pretty
        );
        // $count should be substituted
        assert!(
            !pretty.contains("$count"),
            "Expected $count to be substituted away, got: {}",
            pretty
        );
        assert!(
            pretty.contains("->slice(0, 5)"),
            "Expected ->slice(0, 5), got: {}",
            pretty
        );
    }

    #[test]
    fn test_no_args_on_parameterless_mapping() {
        use apollo_compiler::name;

        let mut registry = MappingRegistry::new();

        let mapping_selection =
            JSONSelection::parse_with_spec("id name", ConnectSpec::V0_5).unwrap();
        let TopLevelSelection::Named(sub) = mapping_selection.inner else {
            panic!("expected Named selection")
        };
        registry.mappings.insert(
            name!(User),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(User),
                parameters: HashSet::new(),
            },
        );

        // Spread without args on parameterless mapping - should work fine
        let selection =
            JSONSelection::parse_with_spec("...User", ConnectSpec::V0_5).unwrap();
        let expanded = registry.expand_selection(&selection).unwrap();
        assert_eq!(expanded.pretty_print(), "id\nname");
    }

    #[test]
    fn test_compute_parameters_from_selection() {
        // Test that compute_parameters correctly identifies non-runtime $vars
        let selection = JSONSelection::parse_with_spec(
            "name: $this.displayName items: items->slice($offset, $limit) config: $config.key",
            ConnectSpec::V0_5,
        )
        .unwrap();

        let params = MappingRegistry::compute_parameters(&selection.inner);

        // $this and $config are runtime variables - should NOT be parameters
        assert!(!params.contains("this"));
        assert!(!params.contains("config"));
        // $offset and $limit are not runtime variables - should be parameters
        assert!(params.contains("offset"));
        assert!(params.contains("limit"));
    }

    #[test]
    fn test_string_argument_substitution() {
        use apollo_compiler::name;

        let mut registry = MappingRegistry::new();

        let mapping_selection = JSONSelection::parse_with_spec(
            "result: data->echo($msg)",
            ConnectSpec::V0_5,
        )
        .unwrap();
        let TopLevelSelection::Named(sub) = mapping_selection.inner else {
            panic!("expected Named selection")
        };
        let mut params = HashSet::new();
        params.insert("msg".to_string());
        registry.mappings.insert(
            name!(Echo),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(Echo),
                parameters: params,
            },
        );

        let selection = JSONSelection::parse_with_spec(
            "...Echo(msg: \"hello world\")",
            ConnectSpec::V0_5,
        )
        .unwrap();
        let expanded = registry.expand_selection(&selection).unwrap();

        let pretty = expanded.pretty_print();
        assert!(
            pretty.contains("\"hello world\""),
            "Expected string literal in expansion, got: {}",
            pretty
        );
    }

    #[test]
    fn test_nested_spreads_with_different_args() {
        use apollo_compiler::name;

        let mut registry = MappingRegistry::new();

        // Inner mapping with parameter
        let inner_selection = JSONSelection::parse_with_spec(
            "items: items->slice(0, $count)",
            ConnectSpec::V0_5,
        )
        .unwrap();
        let TopLevelSelection::Named(sub) = inner_selection.inner else {
            panic!("expected Named selection")
        };
        let mut inner_params = HashSet::new();
        inner_params.insert("count".to_string());
        registry.mappings.insert(
            name!(Inner),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(Inner),
                parameters: inner_params,
            },
        );

        // Outer mapping references Inner with literal args
        let outer_selection = JSONSelection::parse_with_spec(
            "id ...Inner(count: 3)",
            ConnectSpec::V0_5,
        )
        .unwrap();
        let TopLevelSelection::Named(sub) = outer_selection.inner else {
            panic!("expected Named selection")
        };
        registry.mappings.insert(
            name!(Outer),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(Outer),
                parameters: HashSet::new(),
            },
        );

        // Expand Outer
        let selection =
            JSONSelection::parse_with_spec("...Outer", ConnectSpec::V0_5).unwrap();
        let expanded = registry.expand_selection(&selection).unwrap();

        let pretty = expanded.pretty_print();
        assert!(
            pretty.contains("items: items->slice(0, 3)"),
            "Expected nested expansion with count=3, got: {}",
            pretty
        );
    }

    #[test]
    fn test_spread_args_parse_round_trip() {
        // Test that ...User(count: 5) parses and pretty-prints correctly
        let selection = JSONSelection::parse_with_spec(
            "...User(count: 5)",
            ConnectSpec::V0_5,
        )
        .unwrap();

        let pretty = selection.pretty_print();
        assert_eq!(pretty, "...User(count: 5)");
    }

    #[test]
    fn test_spread_args_multiple_values_round_trip() {
        let selection = JSONSelection::parse_with_spec(
            "...Paginated(offset: 0, limit: 10)",
            ConnectSpec::V0_5,
        )
        .unwrap();

        let pretty = selection.pretty_print();
        assert_eq!(pretty, "...Paginated(offset: 0, limit: 10)");
    }

    #[test]
    fn test_spread_no_args_unchanged() {
        // Existing behavior: ...User without parens still works
        let selection =
            JSONSelection::parse_with_spec("...User", ConnectSpec::V0_5).unwrap();
        let pretty = selection.pretty_print();
        assert_eq!(pretty, "...User");
    }

    #[test]
    fn test_nested_forwarding_rejected() {
        use apollo_compiler::name;

        // v1 restriction: spread args must be literals, not $variable references.
        // ...Inner(count: $count) is not allowed — it would require nested forwarding.
        let mut registry = MappingRegistry::new();

        let inner_selection = JSONSelection::parse_with_spec(
            "items: items->slice(0, $count)",
            ConnectSpec::V0_5,
        )
        .unwrap();
        let TopLevelSelection::Named(sub) = inner_selection.inner else {
            panic!("expected Named selection")
        };
        let mut inner_params = HashSet::new();
        inner_params.insert("count".to_string());
        registry.mappings.insert(
            name!(Inner),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(Inner),
                parameters: inner_params,
            },
        );

        // Outer tries to forward $count to Inner — this must fail
        let outer_selection = JSONSelection::parse_with_spec(
            "id ...Inner(count: $count)",
            ConnectSpec::V0_5,
        )
        .unwrap();
        let TopLevelSelection::Named(sub) = outer_selection.inner else {
            panic!("expected Named selection")
        };
        let mut outer_params = HashSet::new();
        outer_params.insert("count".to_string());
        registry.mappings.insert(
            name!(Outer),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(Outer),
                parameters: outer_params,
            },
        );

        // Expanding ...Outer(count: 5) should fail because Outer's selection
        // uses $count as a spread arg value (nested forwarding not allowed in v1)
        let selection = JSONSelection::parse_with_spec(
            "...Outer(count: 5)",
            ConnectSpec::V0_5,
        )
        .unwrap();
        let result = registry.expand_selection(&selection);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("variable/path as argument value"),
            "Expected nested forwarding rejection, got: {}",
            err
        );
    }

    #[test]
    fn test_dynamic_spread_arg_rejected() {
        use apollo_compiler::name;

        // $args.limit as a spread arg value is not allowed in v1
        let mut registry = MappingRegistry::new();

        let mapping_selection = JSONSelection::parse_with_spec(
            "items: items->slice(0, $count)",
            ConnectSpec::V0_5,
        )
        .unwrap();
        let TopLevelSelection::Named(sub) = mapping_selection.inner else {
            panic!("expected Named selection")
        };
        let mut params = HashSet::new();
        params.insert("count".to_string());
        registry.mappings.insert(
            name!(Items),
            MappingDefinition {
                selection: TopLevelSelection::Named(sub),
                source_type: name!(Items),
                parameters: params,
            },
        );

        // ...Items(count: $args.limit) — dynamic arg, should be rejected
        let selection = JSONSelection::parse_with_spec(
            "...Items(count: $args.limit)",
            ConnectSpec::V0_5,
        )
        .unwrap();
        let result = registry.expand_selection(&selection);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("variable/path as argument value"),
            "Expected rejection of dynamic arg, got: {}",
            err
        );
    }

    #[test]
    fn test_from_schema_infers_parameters_and_expands() {
        use apollo_compiler::Schema;

        // Integration test: verify that from_schema() correctly infers parameters
        // from @mapping selection and that expansion with @connect selection works.
        let schema = Schema::parse(
            r#"
            extend schema @link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@mapping"])
            directive @link(url: String, import: [link__Import]) repeatable on SCHEMA
            scalar link__Import
            directive @mapping(selection: String, as: String) repeatable on OBJECT | INTERFACE

            type User @mapping(selection: "id name friends: friends->slice(0, $count)") {
                id: ID!
                name: String!
                friends: [User!]!
            }

            type Query {
                users: [User]
            }
            "#,
            "test.graphql",
        )
        .unwrap();

        let registry = MappingRegistry::from_schema(&schema).unwrap();

        // Verify parameters were inferred
        let mapping = registry.get_mapping("User").unwrap();
        assert!(
            mapping.parameters.contains("count"),
            "Expected 'count' parameter to be inferred, got: {:?}",
            mapping.parameters
        );

        // Verify expansion works
        let connect_selection = JSONSelection::parse_with_spec(
            "...User(count: 5)",
            ConnectSpec::V0_5,
        )
        .unwrap();
        let expanded = registry.expand_selection(&connect_selection).unwrap();
        let pretty = expanded.pretty_print();
        assert!(
            pretty.contains("friends: friends->slice(0, 5)"),
            "Expected expanded selection with count=5, got: {}",
            pretty
        );
    }
}
