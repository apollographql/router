use std::fmt::Display;
use std::fmt::Formatter;
use std::ops::Range;

use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::name;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::schema::ComponentName;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::validation::DiagnosticList;
use apollo_compiler::validation::Valid;
use indexmap::map::Entry;

use crate::ValidFederationSubgraph;
use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::link::DEFAULT_LINK_NAME;
use crate::link::Link;
use crate::link::LinkError;
use crate::link::spec::Identity;
use crate::subgraph::spec::ANY_SCALAR_NAME;
use crate::subgraph::spec::AppliedFederationLink;
use crate::subgraph::spec::CONTEXTFIELDVALUE_SCALAR_NAME;
use crate::subgraph::spec::ENTITIES_QUERY;
use crate::subgraph::spec::ENTITY_UNION_NAME;
use crate::subgraph::spec::FEDERATION_V2_DIRECTIVE_NAMES;
use crate::subgraph::spec::FederationSpecDefinitions;
use crate::subgraph::spec::KEY_DIRECTIVE_NAME;
use crate::subgraph::spec::LinkSpecDefinitions;
use crate::subgraph::spec::SERVICE_SDL_QUERY;
use crate::subgraph::spec::SERVICE_TYPE;

pub mod spec;
pub mod typestate; // TODO: Move here to overwrite Subgraph after API is reasonable

pub struct Subgraph {
    pub name: String,
    pub url: String,
    pub schema: Schema,
}

impl Subgraph {
    pub fn new(name: &str, url: &str, schema_str: &str) -> Result<Self, FederationError> {
        let schema = Schema::parse(schema_str, name)?;
        // TODO: federation-specific validation
        Ok(Self {
            name: name.to_string(),
            url: url.to_string(),
            schema,
        })
    }

    pub fn parse_and_expand(
        name: &str,
        url: &str,
        schema_str: &str,
    ) -> Result<ValidSubgraph, FederationError> {
        let mut schema = Schema::builder()
            .adopt_orphan_extensions()
            .parse(schema_str, name)
            .build()?;

        let mut imported_federation_definitions: Option<FederationSpecDefinitions> = None;
        let mut imported_link_definitions: Option<LinkSpecDefinitions> = None;
        let default_link_name = DEFAULT_LINK_NAME;
        let link_directives = schema
            .schema_definition
            .directives
            .get_all(&default_link_name);

        for directive in link_directives {
            let link_directive = Link::from_directive_application(directive)?;
            if link_directive.url.identity == Identity::federation_identity() {
                if imported_federation_definitions.is_some() {
                    let msg = "invalid graphql schema - multiple @link imports for the federation specification are not supported";
                    return Err(LinkError::BootstrapError(msg.to_owned()).into());
                }

                imported_federation_definitions =
                    Some(FederationSpecDefinitions::from_link(link_directive)?);
            } else if link_directive.url.identity == Identity::link_identity() {
                // user manually imported @link specification
                if imported_link_definitions.is_some() {
                    let msg = "invalid graphql schema - multiple @link imports for the link specification are not supported";
                    return Err(LinkError::BootstrapError(msg.to_owned()).into());
                }

                imported_link_definitions = Some(LinkSpecDefinitions::new(link_directive));
            }
        }

        // generate additional schema definitions
        Self::populate_missing_type_definitions(
            &mut schema,
            imported_federation_definitions,
            imported_link_definitions,
        )?;
        let schema = schema.validate()?;
        Ok(ValidSubgraph {
            name: name.to_owned(),
            url: url.to_owned(),
            schema,
        })
    }

    fn populate_missing_type_definitions(
        schema: &mut Schema,
        imported_federation_definitions: Option<FederationSpecDefinitions>,
        imported_link_definitions: Option<LinkSpecDefinitions>,
    ) -> Result<(), FederationError> {
        // populate @link spec definitions
        let link_spec_definitions = match imported_link_definitions {
            Some(definitions) => definitions,
            None => {
                // need to apply default @link directive for link spec on schema
                let defaults = LinkSpecDefinitions::default();
                schema
                    .schema_definition
                    .make_mut()
                    .directives
                    .push(defaults.applied_link_directive());
                defaults
            }
        };
        Self::populate_missing_link_definitions(schema, link_spec_definitions)?;

        // populate @link federation spec definitions
        let fed_definitions = match imported_federation_definitions {
            Some(definitions) => definitions,
            None => {
                // federation v1 schema or user does not import federation spec
                // need to apply default @link directive for federation spec on schema
                let defaults = FederationSpecDefinitions::default()?;
                schema
                    .schema_definition
                    .make_mut()
                    .directives
                    .push(defaults.applied_link_directive());
                defaults
            }
        };
        Self::populate_missing_federation_directive_definitions(schema, &fed_definitions)?;
        Self::populate_missing_federation_types(schema, &fed_definitions)
    }

    fn populate_missing_link_definitions(
        schema: &mut Schema,
        link_spec_definitions: LinkSpecDefinitions,
    ) -> Result<(), FederationError> {
        let purpose_enum_name = &link_spec_definitions.purpose_enum_name;
        schema
            .types
            .entry(purpose_enum_name.clone())
            .or_insert_with(|| {
                link_spec_definitions
                    .link_purpose_enum_definition(purpose_enum_name.clone())
                    .into()
            });
        let import_scalar_name = &link_spec_definitions.import_scalar_name;
        schema
            .types
            .entry(import_scalar_name.clone())
            .or_insert_with(|| {
                link_spec_definitions
                    .import_scalar_definition(import_scalar_name.clone())
                    .into()
            });
        if let Entry::Vacant(entry) = schema.directive_definitions.entry(DEFAULT_LINK_NAME) {
            entry.insert(link_spec_definitions.link_directive_definition()?.into());
        }
        Ok(())
    }

    fn populate_missing_federation_directive_definitions(
        schema: &mut Schema,
        fed_definitions: &FederationSpecDefinitions,
    ) -> Result<(), FederationError> {
        // scalar FieldSet
        let fieldset_scalar_name = &fed_definitions.fieldset_scalar_name;
        schema
            .types
            .entry(fieldset_scalar_name.clone())
            .or_insert_with(|| {
                fed_definitions
                    .fieldset_scalar_definition(fieldset_scalar_name.clone())
                    .into()
            });

        // scalar ContextFieldValue
        let namespaced_contextfieldvalue_scalar_name =
            fed_definitions.namespaced_type_name(&CONTEXTFIELDVALUE_SCALAR_NAME, false);
        if let Entry::Vacant(entry) = schema
            .types
            .entry(namespaced_contextfieldvalue_scalar_name.clone())
        {
            let type_definition = fed_definitions.contextfieldvalue_scalar_definition(&Some(
                namespaced_contextfieldvalue_scalar_name,
            ));
            entry.insert(type_definition.into());
        }

        for directive_name in &FEDERATION_V2_DIRECTIVE_NAMES {
            let namespaced_directive_name =
                fed_definitions.namespaced_type_name(directive_name, true);
            if let Entry::Vacant(entry) = schema
                .directive_definitions
                .entry(namespaced_directive_name.clone())
            {
                let directive_definition = fed_definitions.directive_definition(
                    directive_name,
                    &Some(namespaced_directive_name.to_owned()),
                )?;
                entry.insert(directive_definition.into());
            }
        }
        Ok(())
    }

    fn populate_missing_federation_types(
        schema: &mut Schema,
        fed_definitions: &FederationSpecDefinitions,
    ) -> Result<(), FederationError> {
        schema
            .types
            .entry(SERVICE_TYPE)
            .or_insert_with(|| fed_definitions.service_object_type_definition());

        let entities = Self::locate_entities(schema, fed_definitions);
        let entities_present = !entities.is_empty();
        if entities_present {
            schema
                .types
                .entry(ENTITY_UNION_NAME)
                .or_insert_with(|| fed_definitions.entity_union_definition(entities));
            schema
                .types
                .entry(ANY_SCALAR_NAME)
                .or_insert_with(|| fed_definitions.any_scalar_definition());
        }

        let query_type_name = schema
            .schema_definition
            .make_mut()
            .query
            .get_or_insert(ComponentName::from(name!("Query")));
        if let ExtendedType::Object(query_type) = schema
            .types
            .entry(query_type_name.name.clone())
            .or_insert(ExtendedType::Object(Node::new(ObjectType {
                description: None,
                name: query_type_name.name.clone(),
                directives: Default::default(),
                fields: IndexMap::default(),
                implements_interfaces: IndexSet::default(),
            })))
        {
            let query_type = query_type.make_mut();
            query_type
                .fields
                .entry(SERVICE_SDL_QUERY)
                .or_insert_with(|| fed_definitions.service_sdl_query_field());
            if entities_present {
                // _entities(representations: [_Any!]!): [_Entity]!
                query_type
                    .fields
                    .entry(ENTITIES_QUERY)
                    .or_insert_with(|| fed_definitions.entities_query_field());
            }
        }
        Ok(())
    }

    fn locate_entities(
        schema: &mut Schema,
        fed_definitions: &FederationSpecDefinitions,
    ) -> IndexSet<ComponentName> {
        let mut entities = Vec::new();
        let immutable_type_map = schema.types.to_owned();
        for (named_type, extended_type) in immutable_type_map.iter() {
            let is_entity = extended_type
                .directives()
                .iter()
                .find(|d| {
                    d.name
                        == fed_definitions
                            .namespaced_type_name(&KEY_DIRECTIVE_NAME, true)
                            .as_str()
                })
                .map(|_| true)
                .unwrap_or(false);
            if is_entity {
                entities.push(named_type);
            }
        }
        let entity_set: IndexSet<ComponentName> =
            entities.iter().map(|e| ComponentName::from(*e)).collect();
        entity_set
    }
}

impl std::fmt::Debug for Subgraph {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, r#"name: {}, urL: {}"#, self.name, self.url)
    }
}

pub struct ValidSubgraph {
    pub name: String,
    pub url: String,
    pub schema: Valid<Schema>,
}

impl std::fmt::Debug for ValidSubgraph {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, r#"name: {}, url: {}"#, self.name, self.url)
    }
}

impl From<ValidFederationSubgraph> for ValidSubgraph {
    fn from(value: ValidFederationSubgraph) -> Self {
        Self {
            name: value.name,
            url: value.url,
            schema: value.schema.schema().clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SingleSubgraphError {
    pub(crate) error: SingleFederationError,
    pub(crate) locations: Vec<Range<LineColumn>>,
}

/// Currently, this is making up for the fact that we don't have an equivalent of `addSubgraphToErrors`.
/// In JS, that manipulates the underlying `GraphQLError` message to prepend the subgraph name. In Rust,
/// it's idiomatic to have strongly typed errors which defer conversion to strings via `thiserror`, so
/// for now we wrap the underlying error until we figure out a longer-term replacement that accounts
/// for missing error codes and the like.
#[derive(Clone, Debug)]
pub struct SubgraphError {
    pub(crate) subgraph: String,
    pub(crate) errors: Vec<SingleSubgraphError>,
}

impl SubgraphError {
    // Legacy constructor without locations info.
    pub(crate) fn new_without_locations(
        subgraph: impl Into<String>,
        error: impl Into<FederationError>,
    ) -> Self {
        let subgraph = subgraph.into();
        let error: FederationError = error.into();
        SubgraphError {
            subgraph,
            errors: error
                .errors()
                .into_iter()
                .map(|e| SingleSubgraphError {
                    error: e.clone(),
                    locations: Vec::new(),
                })
                .collect(),
        }
    }

    /// Construct from a FederationError.
    ///
    /// Note: FederationError may hold multiple errors. In that case, all individual errors in the
    ///       FederationError will share the same locations.
    #[allow(dead_code)]
    pub(crate) fn from_federation_error(
        subgraph: impl Into<String>,
        error: impl Into<FederationError>,
        locations: Vec<Range<LineColumn>>,
    ) -> Self {
        let error: FederationError = error.into();
        let errors = error
            .errors()
            .into_iter()
            .map(|e| SingleSubgraphError {
                error: e.clone(),
                locations: locations.clone(),
            })
            .collect();
        SubgraphError {
            subgraph: subgraph.into(),
            errors,
        }
    }

    /// Constructing from GraphQL errors.
    pub(crate) fn from_diagnostic_list(
        subgraph: impl Into<String>,
        errors: DiagnosticList,
    ) -> Self {
        let subgraph = subgraph.into();
        SubgraphError {
            subgraph,
            errors: errors
                .iter()
                .map(|d| SingleSubgraphError {
                    error: SingleFederationError::InvalidGraphQL {
                        message: d.to_string(),
                    },
                    locations: d.line_column_range().iter().cloned().collect(),
                })
                .collect(),
        }
    }

    /// Convert SubgraphError to FederationError.
    /// * WARNING: This is a lossy conversion, losing location information.
    pub(crate) fn into_federation_error(self) -> FederationError {
        MultipleFederationErrors::from_iter(self.errors.into_iter().map(|e| e.error)).into()
    }

    // Format subgraph errors in the same way as `Rover` does.
    // And return them as a vector of (error_code, error_message) tuples
    // - Gather associated errors from the validation error.
    // - Split each error into its code and message.
    // - Add the subgraph name prefix to CompositionError message.
    //
    // This is mainly for internal testing. Consider using `to_composition_errors` method instead.
    pub fn format_errors(&self) -> Vec<(String, String)> {
        self.errors
            .iter()
            .map(|e| {
                let error = &e.error;
                (
                    error.code_string(),
                    format!("[{subgraph}] {error}", subgraph = self.subgraph),
                )
            })
            .collect()
    }
}

impl Display for SubgraphError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let subgraph = &self.subgraph;
        for (code, message) in self.format_errors() {
            writeln!(f, "{code} [{subgraph}] {message}")?;
        }
        Ok(())
    }
}

pub mod test_utils {
    use super::SubgraphError;
    use super::typestate::Expanded;
    use super::typestate::Subgraph;
    use super::typestate::Validated;

    pub enum BuildOption {
        AsIs,
        AsFed2,
    }

    pub fn build_inner(
        schema_str: &str,
        build_option: BuildOption,
    ) -> Result<Subgraph<Validated>, SubgraphError> {
        let name = "S";
        let subgraph =
            Subgraph::parse(name, &format!("http://{name}"), schema_str).expect("valid schema");
        let subgraph = if matches!(build_option, BuildOption::AsFed2) {
            subgraph.into_fed2_test_subgraph(true, false)?
        } else {
            subgraph
        };
        let mut subgraph = subgraph.expand_links()?.assume_upgraded();
        subgraph.normalize_root_types()?;
        subgraph.validate()
    }

    pub fn build_inner_expanded(
        schema_str: &str,
        build_option: BuildOption,
    ) -> Result<Subgraph<Expanded>, SubgraphError> {
        let name = "S";
        let subgraph =
            Subgraph::parse(name, &format!("http://{name}"), schema_str).expect("valid schema");
        let subgraph = if matches!(build_option, BuildOption::AsFed2) {
            subgraph.into_fed2_test_subgraph(true, false)?
        } else {
            subgraph
        };
        subgraph.expand_links()
    }

    pub fn build_and_validate(schema_str: &str) -> Subgraph<Validated> {
        build_inner(schema_str, BuildOption::AsIs).expect("expanded subgraph to be valid")
    }

    pub fn build_and_expand(schema_str: &str) -> Subgraph<Expanded> {
        build_inner_expanded(schema_str, BuildOption::AsIs).expect("expanded subgraph to be valid")
    }

    pub fn build_for_errors_with_option(
        schema: &str,
        build_option: BuildOption,
    ) -> Vec<(String, String)> {
        build_inner(schema, build_option)
            .expect_err("subgraph error was expected")
            .format_errors()
    }

    /// Build subgraph expecting errors, assuming fed 2.
    pub fn build_for_errors(schema: &str) -> Vec<(String, String)> {
        build_for_errors_with_option(schema, BuildOption::AsFed2)
    }

    pub fn remove_indentation(s: &str) -> String {
        // count the last lines that are space-only
        let first_empty_lines = s.lines().take_while(|line| line.trim().is_empty()).count();
        let last_empty_lines = s
            .lines()
            .rev()
            .take_while(|line| line.trim().is_empty())
            .count();

        // lines without the space-only first/last lines
        let lines = s
            .lines()
            .skip(first_empty_lines)
            .take(s.lines().count() - first_empty_lines - last_empty_lines);

        // compute the indentation
        let indentation = lines
            .clone()
            .map(|line| line.chars().take_while(|c| *c == ' ').count())
            .min()
            .unwrap_or(0);

        // remove the indentation
        lines
            .map(|line| {
                line.trim_end()
                    .chars()
                    .skip(indentation)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// True if a and b contain the same error messages
    pub fn check_errors(a: &[(String, String)], b: &[(&str, &str)]) -> Result<(), String> {
        if a.len() != b.len() {
            return Err(format!(
                "Mismatched error counts: {} != {}\n\nexpected:\n{}\n\nactual:\n{}",
                b.len(),
                a.len(),
                b.iter()
                    .map(|(code, msg)| { format!("- {code}: {msg}") })
                    .collect::<Vec<_>>()
                    .join("\n"),
                a.iter()
                    .map(|(code, msg)| { format!("+ {code}: {msg}") })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ));
        }

        // remove indentations from messages to ignore indentation differences
        let b_iter = b
            .iter()
            .map(|(code, message)| (*code, remove_indentation(message)));
        let diff: Vec<_> = a
            .iter()
            .map(|(code, message)| (code.as_str(), remove_indentation(message)))
            .zip(b_iter)
            .filter(|(a_i, b_i)| a_i.0 != b_i.0 || a_i.1 != b_i.1)
            .collect();
        if diff.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "Mismatched errors:\n{}\n",
                diff.iter()
                    .map(|(a_i, b_i)| { format!("- {}: {}\n+ {}: {}", b_i.0, b_i.1, a_i.0, a_i.1) })
                    .collect::<Vec<_>>()
                    .join("\n")
            ))
        }
    }

    #[macro_export]
    macro_rules! assert_errors {
        ($a:expr, $b:expr) => {
            match apollo_federation::subgraph::test_utils::check_errors(&$a, &$b) {
                Ok(()) => {
                    // Success
                }
                Err(e) => {
                    panic!("{e}")
                }
            }
        };
    }
}

// INTERNAL: For use by Language Server Protocol (LSP) team
// WARNING: Any changes to this function signature will result in breakages in the dependency chain
// Generates a diff string containing directives and types not included in initial schema string
pub fn schema_diff_expanded_from_initial(schema_str: String) -> Result<String, FederationError> {
    // Parse schema string as Schema
    let initial_schema = Schema::parse(schema_str, "")?;

    // Initialize and expand subgraph, without validation
    let initial_subgraph = typestate::Subgraph::new("S", "http://S", initial_schema.clone());
    let expanded_subgraph = initial_subgraph
        .expand_links()
        .map_err(|e| e.into_federation_error())?;

    // Build string of missing directives and types from initial to expanded
    let mut diff = String::new();

    // Push newly added directives onto diff
    for (dir_name, dir_def) in &expanded_subgraph.schema().schema().directive_definitions {
        if !initial_schema.directive_definitions.contains_key(dir_name) {
            diff.push_str(&dir_def.to_string());
            diff.push('\n');
        }
    }

    // Push newly added types onto diff
    for (named_ty, extended_ty) in &expanded_subgraph.schema().schema().types {
        if !initial_schema.types.contains_key(named_ty) {
            diff.push_str(&extended_ty.to_string());
        }
    }

    Ok(diff)
}

#[cfg(test)]
mod tests {
    use crate::subgraph::schema_diff_expanded_from_initial;

    #[test]
    fn returns_correct_schema_diff_for_fed_2_0() {
        let schema_string = r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v2.0")

                type Query {
                    s: String
                }"#
        .to_string();

        let diff = schema_diff_expanded_from_initial(schema_string);

        insta::assert_snapshot!(diff.unwrap_or_default(), @r#"directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
directive @federation__key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE
directive @federation__requires(fields: federation__FieldSet!) on FIELD_DEFINITION
directive @federation__provides(fields: federation__FieldSet!) on FIELD_DEFINITION
directive @federation__external(reason: String) on OBJECT | FIELD_DEFINITION
directive @federation__tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION
directive @federation__extends on OBJECT | INTERFACE
directive @federation__shareable on OBJECT | FIELD_DEFINITION
directive @federation__inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION
directive @federation__override(from: String!) on FIELD_DEFINITION
enum link__Purpose {
  """
  `SECURITY` features provide metadata necessary to securely resolve fields.
  """
  SECURITY
  """
  `EXECUTION` features provide metadata necessary for operation execution.
  """
  EXECUTION
}
scalar link__Import
scalar federation__FieldSet
scalar _Any
type _Service {
  sdl: String
}"#);
    }

    #[test]
    fn returns_correct_schema_diff_for_fed_2_4() {
        let schema_string = r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v2.4")

                type Query {
                    s: String
                }"#
        .to_string();

        let diff = schema_diff_expanded_from_initial(schema_string);

        insta::assert_snapshot!(diff.unwrap_or_default(), @r#"directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
directive @federation__key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE
directive @federation__requires(fields: federation__FieldSet!) on FIELD_DEFINITION
directive @federation__provides(fields: federation__FieldSet!) on FIELD_DEFINITION
directive @federation__external(reason: String) on OBJECT | FIELD_DEFINITION
directive @federation__tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA
directive @federation__extends on OBJECT | INTERFACE
directive @federation__shareable repeatable on OBJECT | FIELD_DEFINITION
directive @federation__inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION
directive @federation__override(from: String!) on FIELD_DEFINITION
directive @federation__composeDirective(name: String) repeatable on SCHEMA
directive @federation__interfaceObject on OBJECT
enum link__Purpose {
  """
  `SECURITY` features provide metadata necessary to securely resolve fields.
  """
  SECURITY
  """
  `EXECUTION` features provide metadata necessary for operation execution.
  """
  EXECUTION
}
scalar link__Import
scalar federation__FieldSet
scalar _Any
type _Service {
  sdl: String
}"#);
    }

    #[test]
    fn returns_correct_schema_diff_for_fed_2_9() {
        let schema_string = r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v2.9")

                type Query {
                    s: String
                }"#
        .to_string();

        let diff = schema_diff_expanded_from_initial(schema_string);

        insta::assert_snapshot!(diff.unwrap_or_default(), @r#"directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
directive @federation__key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE
directive @federation__requires(fields: federation__FieldSet!) on FIELD_DEFINITION
directive @federation__provides(fields: federation__FieldSet!) on FIELD_DEFINITION
directive @federation__external(reason: String) on OBJECT | FIELD_DEFINITION
directive @federation__tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA
directive @federation__extends on OBJECT | INTERFACE
directive @federation__shareable repeatable on OBJECT | FIELD_DEFINITION
directive @federation__inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION
directive @federation__override(from: String!, label: String) on FIELD_DEFINITION
directive @federation__composeDirective(name: String) repeatable on SCHEMA
directive @federation__interfaceObject on OBJECT
directive @federation__authenticated on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM
directive @federation__requiresScopes(scopes: [[federation__Scope!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM
directive @federation__policy(policies: [[federation__Policy!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM
directive @federation__context(name: String!) repeatable on INTERFACE | OBJECT | UNION
directive @federation__fromContext(field: federation__ContextFieldValue) on ARGUMENT_DEFINITION
directive @federation__cost(weight: Int!) on ARGUMENT_DEFINITION | ENUM | FIELD_DEFINITION | INPUT_FIELD_DEFINITION | OBJECT | SCALAR
directive @federation__listSize(assumedSize: Int, slicingArguments: [String!], sizedFields: [String!], requireOneSlicingArgument: Boolean = true) on FIELD_DEFINITION
enum link__Purpose {
  """
  `SECURITY` features provide metadata necessary to securely resolve fields.
  """
  SECURITY
  """
  `EXECUTION` features provide metadata necessary for operation execution.
  """
  EXECUTION
}
scalar link__Import
scalar federation__FieldSet
scalar federation__Scope
scalar federation__Policy
scalar federation__ContextFieldValue
scalar _Any
type _Service {
  sdl: String
}"#);
    }
}
