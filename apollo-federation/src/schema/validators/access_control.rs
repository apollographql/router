use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::LazyLock;

use apollo_compiler::Name;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Value;

use crate::bail;
use crate::error::CompositionError;
use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::internal_error;
use crate::link::authenticated_spec_definition::AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC;
use crate::link::authenticated_spec_definition::AUTHENTICATED_VERSIONS;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::link::join_spec_definition::JoinSpecDefinition;
use crate::link::policy_spec_definition::POLICY_DIRECTIVE_NAME_IN_SPEC;
use crate::link::policy_spec_definition::POLICY_POLICIES_ARGUMENT_NAME;
use crate::link::policy_spec_definition::POLICY_VERSIONS;
use crate::link::requires_scopes_spec_definition::REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::requires_scopes_spec_definition::REQUIRES_SCOPES_SCOPES_ARGUMENT_NAME;
use crate::link::requires_scopes_spec_definition::REQUIRES_SCOPES_VERSIONS;
use crate::link::spec::Identity;
use crate::link::spec_definition::SpecDefinition;
use crate::operation::FieldSelection;
use crate::operation::Selection;
use crate::operation::SelectionSet;
use crate::schema::FederationSchema;
use crate::schema::ValidFederationSchema;
use crate::schema::argument_composition_strategies::dnf_conjunction;
use crate::schema::field_set::parse_field_set;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::HasAppliedDirectives;
use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
use crate::schema::subgraph_metadata::SubgraphMetadata;
use crate::subgraph::SubgraphError;
use crate::subgraph::spec::CONTEXT_DIRECTIVE_NAME;
use crate::subgraph::spec::FROM_CONTEXT_DIRECTIVE_NAME;
use crate::subgraph::spec::REQUIRES_DIRECTIVE_NAME;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;

pub(crate) fn validate_no_access_control_on_interfaces(
    schema: &ValidFederationSchema,
    metadata: &SubgraphMetadata,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    let federation_spec = get_federation_spec_definition_from_subgraph(schema)?;
    for directive in [
        AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC,
        REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC,
        POLICY_DIRECTIVE_NAME_IN_SPEC,
    ] {
        if let Some(directive_name) =
            federation_spec.directive_name_in_schema(schema, &directive)?
        {
            let references = schema.referencers().get_directive(&directive_name);
            for interface_field in &references.interface_fields {
                errors
                    .errors
                    .push(SingleFederationError::AuthRequirementsAppliedOnInterface {
                        directive_name: directive_name.to_string(),
                        coordinate: interface_field.to_string(),
                        kind: "field".to_string(),
                    })
            }
            for interface_type in &references.interface_types {
                errors
                    .errors
                    .push(SingleFederationError::AuthRequirementsAppliedOnInterface {
                        directive_name: directive_name.to_string(),
                        coordinate: interface_type.to_string(),
                        kind: "interface".to_string(),
                    })
            }
            for object_type in &references.object_types {
                if metadata.is_interface_object_type(&object_type.type_name) {
                    errors
                        .errors
                        .push(SingleFederationError::AuthRequirementsAppliedOnInterface {
                            directive_name: directive_name.to_string(),
                            coordinate: object_type.to_string(),
                            kind: "interface object".to_string(),
                        })
                }
            }
        }
    }
    Ok(())
}

// need to verify usage of @requires and @fromContext on fields that require authorization
pub(crate) fn validate_transitive_access_control_requirements_in_the_supergraph(
    join_spec_definition: &JoinSpecDefinition,
    subgraph_names_to_join_spec_name: &HashMap<String, Name>,
    supergraph_schema: &FederationSchema,
    subgraphs: &[Subgraph<Validated>],
    errors: &mut Vec<CompositionError>,
) -> Result<(), FederationError> {
    let mut fields_with_requires: HashSet<ObjectOrInterfaceFieldDefinitionPosition> =
        HashSet::default();
    let mut fields_with_from_context: HashSet<ObjectOrInterfaceFieldDefinitionPosition> =
        HashSet::default();
    // first we capture locations where @requires and @fromContext is applied as those will be
    // converted to arguments to @join__field in the supergraph
    for subgraph in subgraphs.iter() {
        let requires_directive_name = &subgraph
            .metadata()
            .federation_spec_definition()
            .requires_directive_definition(subgraph.schema())?
            .name;
        let requires_referencers = subgraph
            .schema()
            .referencers
            .get_directive(requires_directive_name);
        // @requires should only be present in the subgraphs on the object fields
        // but in the supergraph it could be either on object field or interface (object) field
        for field in &requires_referencers.object_fields {
            fields_with_requires.insert(field.clone().into());
        }

        let from_context_directive_name = &subgraph
            .metadata()
            .federation_spec_definition()
            .from_context_directive_definition(subgraph.schema())?
            .name;
        let from_context_referencers = subgraph
            .schema()
            .referencers()
            .get_directive(from_context_directive_name);
        // @fromContext should only be present in the subgraphs on the object field arguments
        // but in the supergraph it can be either on object field or interface (object) field
        for field in &from_context_referencers.object_field_arguments {
            fields_with_from_context.insert(field.parent().into());
        }
    }

    let validator = AccessControlValidator::new(
        supergraph_schema,
        join_spec_definition,
        subgraph_names_to_join_spec_name,
    )?;
    for requires_coordinate in fields_with_requires {
        errors.extend(validator.validate_requires(requires_coordinate)?);
    }
    for context_coordinate in fields_with_from_context {
        errors.extend(validator.validate_from_context(context_coordinate)?);
    }
    Ok(())
}

struct AccessControlValidator<'validator> {
    valid_schema: ValidFederationSchema,
    join_spec_names_to_subgraph_names: HashMap<Name, String>,
    join_spec_definition: &'validator JoinSpecDefinition,
    join_field_directive_name: Name,
    authenticated_directive_name: Option<Name>,
    requires_scopes_directive_name: Option<Name>,
    policy_directive_name: Option<Name>,
    contexts: HashMap<String, HashSet<String>>,
}

impl<'validator> AccessControlValidator<'validator> {
    fn new(
        supergraph_schema: &FederationSchema,
        join_spec_definition: &'validator JoinSpecDefinition,
        subgraph_names_to_join_spec_name: &HashMap<String, Name>,
    ) -> Result<Self, FederationError> {
        let valid_schema = ValidFederationSchema::new_assume_valid(supergraph_schema.clone())
            .map_err(|(_, err)| err)?;
        let join_spec_names_to_subgraph_names: HashMap<Name, String> =
            subgraph_names_to_join_spec_name
                .iter()
                .map(|(k, v)| (v.clone(), k.clone()))
                .collect();
        let join_field_directive_name = join_spec_definition
            .field_directive_definition(supergraph_schema)?
            .name
            .clone();

        let Some(links_metadata) = supergraph_schema.metadata() else {
            bail!("Missing links metadata in supergraph schema");
        };

        // TODO could this be passed from merger?
        let authenticated_directive_name = links_metadata
            .for_identity(&Identity::authenticated_identity())
            .and_then(|authenticated_spec| {
                AUTHENTICATED_VERSIONS
                    .find(&authenticated_spec.url.version)
                    .and_then(|authenticated_definition| {
                        authenticated_definition
                            .directive_name_in_schema(
                                supergraph_schema,
                                &AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC,
                            )
                            .ok()
                    })
            })
            .flatten();
        let requires_scopes_directive_name = links_metadata
            .for_identity(&Identity::requires_scopes_identity())
            .and_then(|requires_scopes_spec| {
                REQUIRES_SCOPES_VERSIONS
                    .find(&requires_scopes_spec.url.version)
                    .and_then(|requires_scopes_definition| {
                        requires_scopes_definition
                            .directive_name_in_schema(
                                supergraph_schema,
                                &REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC,
                            )
                            .ok()
                    })
            })
            .flatten();
        let policy_directive_name = links_metadata
            .for_identity(&Identity::policy_identity())
            .and_then(|policy_spec| {
                POLICY_VERSIONS
                    .find(&policy_spec.url.version)
                    .and_then(|policy_definition| {
                        policy_definition
                            .directive_name_in_schema(
                                supergraph_schema,
                                &POLICY_DIRECTIVE_NAME_IN_SPEC,
                            )
                            .ok()
                    })
            })
            .flatten();

        let mut contexts: HashMap<String, HashSet<String>> = HashMap::default();
        if let Some(context_spec_link) = links_metadata.for_identity(&Identity::context_identity())
        {
            let context_directive_name =
                context_spec_link.directive_name_in_schema(&CONTEXT_DIRECTIVE_NAME);
            let references = supergraph_schema
                .referencers
                .get_directive(&context_directive_name);
            for object_type in &references.object_types {
                for context_directive in
                    object_type.get_applied_directives(supergraph_schema, &context_directive_name)
                {
                    let Some(context_name) = context_directive
                        .specified_argument_by_name("name")
                        .and_then(|v| v.as_str())
                    else {
                        bail!("@context directive should specify name argument");
                    };
                    contexts
                        .entry(context_name.to_string())
                        .or_default()
                        .insert(object_type.to_string());
                }
            }
        }

        Ok(Self {
            valid_schema,
            join_spec_names_to_subgraph_names,
            join_spec_definition,
            join_field_directive_name,
            authenticated_directive_name,
            requires_scopes_directive_name,
            policy_directive_name,
            contexts,
        })
    }

    pub(crate) fn validate_requires(
        &self,
        requires_position: ObjectOrInterfaceFieldDefinitionPosition,
    ) -> Result<Vec<CompositionError>, FederationError> {
        let auth_requirements_on_requires = self
            .calculate_auth_requirements_to_verify(&requires_position, &REQUIRES_DIRECTIVE_NAME);
        let join_directives_on_requires = requires_position
            .get_applied_directives(&self.valid_schema.schema, &self.join_field_directive_name);
        let mut errors = vec![];
        for join_directive_on_requires in &join_directives_on_requires {
            let join_field_args = &self
                .join_spec_definition
                .field_directive_arguments(join_directive_on_requires)?;
            if let Some(requires_field_set) = join_field_args.requires {
                let field_set = parse_field_set(
                    &self.valid_schema,
                    requires_position.type_name().into(),
                    requires_field_set,
                    false,
                )?;
                if let Err(e) = self.verify_auth_requirements_on_selection_set(
                    &field_set,
                    &auth_requirements_on_requires,
                ) {
                    errors.extend(
                        self.enhance_error_message_with_subgraph_info(e, &join_field_args.graph),
                    );
                };
            }
        }
        Ok(errors)
    }

    pub(crate) fn validate_from_context(
        &self,
        context_position: ObjectOrInterfaceFieldDefinitionPosition,
    ) -> Result<Vec<CompositionError>, FederationError> {
        let auth_requirements_on_context = self
            .calculate_auth_requirements_to_verify(&context_position, &FROM_CONTEXT_DIRECTIVE_NAME);
        let join_directives_on_from_context = context_position
            .get_applied_directives(&self.valid_schema.schema, &self.join_field_directive_name);
        let mut errors = vec![];
        for join_directive_on_from_context in join_directives_on_from_context {
            let join_field_args = &self
                .join_spec_definition
                .field_directive_arguments(join_directive_on_from_context)?;
            if let Some(context_arguments) = &join_field_args.context_arguments {
                for context_arg in context_arguments {
                    if let Some(target_type_names) = self.contexts.get(context_arg.context) {
                        // we need to verify against all possible contexts
                        for target_type_name in target_type_names {
                            let target_type = self
                                .valid_schema
                                .get_type(Name::new_unchecked(target_type_name))?;
                            let target_type_auth_requirements =
                                self.read_auth_requirements_from_element(target_type.clone());
                            if !auth_requirements_on_context
                                .satisfies(target_type_auth_requirements)
                            {
                                let error =
                                    SingleFederationError::MissingTransitiveAuthRequirements {
                                        message: format!(
                                            "Field \"{context_position}\" does not specify necessary @authenticated, @requiresScopes and/or @policy auth requirements to access the transitive data in context {} from @fromContext selection set.",
                                            context_arg.context
                                        ),
                                    };
                                errors.extend(self.enhance_error_message_with_subgraph_info(
                                    error,
                                    &join_field_args.graph,
                                ));
                            } else {
                                let context_selection_set = parse_field_set(
                                    &self.valid_schema,
                                    target_type.type_name().into(),
                                    context_arg.selection,
                                    false,
                                )?;
                                if let Err(e) = self.verify_auth_requirements_on_selection_set(
                                    &context_selection_set,
                                    &auth_requirements_on_context,
                                ) {
                                    errors.extend(self.enhance_error_message_with_subgraph_info(
                                        e,
                                        &join_field_args.graph,
                                    ));
                                };
                            }
                        }
                    } else {
                        bail!(
                            "Requested @context \"{}\" does not exist in the schema",
                            context_arg.context
                        );
                    }
                }
            }
        }
        Ok(errors)
    }

    fn calculate_auth_requirements_to_verify(
        &self,
        requires_position: &ObjectOrInterfaceFieldDefinitionPosition,
        target_directive: &Name,
    ) -> AuthRequirements {
        let requires_authenticated =
            self.authenticated_directive_name
                .clone()
                .is_some_and(|directive_name| {
                    let is_field_authenticated = !requires_position
                        .get_applied_directives(&self.valid_schema.schema, &directive_name)
                        .is_empty();
                    let is_type_authenticated = !requires_position
                        .parent()
                        .get_applied_directives(&self.valid_schema.schema, &directive_name)
                        .is_empty();
                    is_field_authenticated || is_type_authenticated
                });
        let required_scopes: Option<BTreeSet<BTreeSet<String>>> = self
            .requires_scopes_directive_name
            .clone()
            .and_then(|directive_name| {
                calculate_disjunction_value(
                    requires_position,
                    &self.valid_schema.schema,
                    &directive_name,
                    &REQUIRES_SCOPES_SCOPES_ARGUMENT_NAME,
                )
            });
        let required_policies: Option<BTreeSet<BTreeSet<String>>> = self
            .policy_directive_name
            .clone()
            .and_then(|directive_name| {
                calculate_disjunction_value(
                    requires_position,
                    &self.valid_schema.schema,
                    &directive_name,
                    &POLICY_POLICIES_ARGUMENT_NAME,
                )
            });
        AuthRequirements {
            field_coordinate: requires_position.to_string(),
            directive: target_directive.clone(),
            requirements: AuthRequirementsOnElement {
                is_authenticated: requires_authenticated,
                policies: required_policies,
                scopes: required_scopes,
            },
        }
    }

    fn verify_auth_requirements_on_selection_set(
        &self,
        selection_set: &SelectionSet,
        auth_requirements: &AuthRequirements,
    ) -> Result<(), FederationError> {
        for selection in selection_set.selections.values() {
            match selection {
                Selection::Field(field_selection) => {
                    self.verify_auth_on_field_selection(field_selection, auth_requirements)?;
                    if let Some(field_subselection) = &field_selection.selection_set {
                        self.verify_auth_requirements_on_selection_set(
                            field_subselection,
                            auth_requirements,
                        )?;
                    }
                }
                Selection::InlineFragment(inline_selection) => {
                    if let Some(condition) =
                        &inline_selection.inline_fragment.type_condition_position
                    {
                        self.verify_auth_on_type_condition(condition, auth_requirements)?;
                    }
                    self.verify_auth_requirements_on_selection_set(
                        &inline_selection.selection_set,
                        auth_requirements,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn verify_auth_on_field_selection(
        &self,
        field_selection: &FieldSelection,
        auth_requirements: &AuthRequirements,
    ) -> Result<(), FederationError> {
        let field_position = &field_selection.field.field_position;
        let field_reqs = self.read_auth_requirements_from_element(field_position.clone());

        let field_return_type_position = field_selection.field.output_base_type()?;
        let field_return_type_reqs =
            self.read_auth_requirements_from_element(field_return_type_position.clone());

        if !auth_requirements.satisfies(field_reqs)
            || !auth_requirements.satisfies(field_return_type_reqs)
        {
            Err(FederationError::SingleFederationError(
                SingleFederationError::MissingTransitiveAuthRequirements {
                    message: format!(
                        "Field \"{}\" does not specify necessary @authenticated, @requiresScopes and/or @policy auth requirements to access the transitive field \"{field_position}\" data from @{} selection set.",
                        auth_requirements.field_coordinate, auth_requirements.directive
                    ),
                },
            ))
        } else {
            Ok(())
        }
    }

    fn verify_auth_on_type_condition(
        &self,
        condition_position: &CompositeTypeDefinitionPosition,
        auth_requirements: &AuthRequirements,
    ) -> Result<(), FederationError> {
        let condition_reqs = self.read_auth_requirements_from_element(condition_position.clone());
        if !auth_requirements.satisfies(condition_reqs) {
            Err(FederationError::SingleFederationError(
                SingleFederationError::MissingTransitiveAuthRequirements {
                    message: format!(
                        "Field \"{}\" does not specify necessary @authenticated, @requiresScopes and/or @policy auth requirements to access the transitive field \"{condition_position}\" data from @{} selection set.",
                        auth_requirements.field_coordinate, auth_requirements.directive
                    ),
                },
            ))
        } else {
            Ok(())
        }
    }

    fn read_auth_requirements_from_element<T: HasAppliedDirectives>(
        &self,
        element: T,
    ) -> Option<AuthRequirementsOnElement> {
        let requires_authenticated =
            self.authenticated_directive_name
                .clone()
                .is_some_and(|directive_name| {
                    !element
                        .get_applied_directives(&self.valid_schema.schema, &directive_name)
                        .is_empty()
                });
        let required_scopes: Option<BTreeSet<BTreeSet<String>>> = self
            .requires_scopes_directive_name
            .clone()
            .and_then(|directive_name| {
                parse_optional_disjunction_value_from_element(
                    &element,
                    &self.valid_schema.schema,
                    &directive_name,
                    &REQUIRES_SCOPES_SCOPES_ARGUMENT_NAME,
                )
            });
        let required_policies: Option<BTreeSet<BTreeSet<String>>> = self
            .policy_directive_name
            .clone()
            .and_then(|directive_name| {
                parse_optional_disjunction_value_from_element(
                    &element,
                    &self.valid_schema.schema,
                    &directive_name,
                    &POLICY_POLICIES_ARGUMENT_NAME,
                )
            });
        if requires_authenticated || required_scopes.is_some() || required_policies.is_some() {
            Some(AuthRequirementsOnElement {
                is_authenticated: requires_authenticated,
                scopes: required_scopes,
                policies: required_policies,
            })
        } else {
            None
        }
    }

    fn enhance_error_message_with_subgraph_info(
        &self,
        error: impl Into<FederationError>,
        target_subgraph: &Option<Name>,
    ) -> Vec<CompositionError> {
        if let Some(subgraph_name) = target_subgraph
            .clone()
            .and_then(|s| self.join_spec_names_to_subgraph_names.get(&s))
        {
            let subgraph_error = SubgraphError::new_without_locations(subgraph_name, error.into());
            subgraph_error.to_composition_errors().collect()
        } else {
            // should only ever be a SingleFederationError but just in case....
            error
                .into()
                .into_errors()
                .iter()
                .map(|e| CompositionError::MergeValidationError { error: e.clone() })
                .collect()
        }
    }
}

fn calculate_disjunction_value(
    position: &ObjectOrInterfaceFieldDefinitionPosition,
    schema: &FederationSchema,
    directive_name: &Name,
    argument_name: &Name,
) -> Option<BTreeSet<BTreeSet<String>>> {
    let mut to_merge = vec![];
    if let Some(val) = position
        .get_applied_directives(schema, directive_name)
        .first()
        .and_then(|directive| read_disjunction_argument_value(argument_name, directive).ok())
    {
        to_merge.push(val.clone());
    }
    if let Some(val) = position
        .parent()
        .get_applied_directives(schema, directive_name)
        .first()
        .and_then(|directive| read_disjunction_argument_value(argument_name, directive).ok())
    {
        to_merge.push(val.clone());
    }

    if to_merge.is_empty() {
        None
    } else {
        let merged = dnf_conjunction(&to_merge);
        Some(parse_disjunction_value(&merged))
    }
}

fn parse_optional_disjunction_value_from_element<T: HasAppliedDirectives>(
    element: &T,
    schema: &FederationSchema,
    directive_name: &Name,
    argument_name: &Name,
) -> Option<BTreeSet<BTreeSet<String>>> {
    element
        .get_applied_directives(schema, directive_name)
        .first()
        .and_then(|directive| {
            read_disjunction_argument_value(argument_name, directive.as_ref()).ok()
        })
        .map(parse_disjunction_value)
}

fn read_disjunction_argument_value<'directive>(
    argument_name: &Name,
    application: &'directive Directive,
) -> Result<&'directive Value, FederationError> {
    application
        .specified_argument_by_name(argument_name)
        .ok_or_else(|| {
            internal_error!(
                "Required argument \"{argument_name}\" of directive \"@{}\" was not present.",
                application.name
            )
        })
        .map(|v| v.as_ref())
}

fn parse_disjunction_value(value: &Value) -> BTreeSet<BTreeSet<String>> {
    value.as_list().map_or_else(
        || EMPTY_DNF_SET.clone(),
        |disjunctions| {
            disjunctions
                .iter()
                .map(|d| {
                    d.as_ref()
                        .as_list()
                        .map_or_else(BTreeSet::default, |conjunctions| {
                            conjunctions.iter().map(|c| c.to_string()).collect()
                        })
                })
                .collect()
        },
    )
}

static EMPTY_DNF_SET: LazyLock<BTreeSet<BTreeSet<String>>> = LazyLock::new(|| {
    let mut set = BTreeSet::new();
    set.insert(BTreeSet::default());
    set
});

#[derive(Debug)]
struct AuthRequirements {
    field_coordinate: String,
    directive: Name,
    requirements: AuthRequirementsOnElement,
}

impl AuthRequirements {
    fn satisfies(&self, other: Option<AuthRequirementsOnElement>) -> bool {
        // auth requirements on element have to be an implication of type + field requirements
        other.map_or_else(|| true, |o| self.requirements.satisfies(o))
    }
}

#[derive(Clone, Debug)]
struct AuthRequirementsOnElement {
    is_authenticated: bool,
    scopes: Option<BTreeSet<BTreeSet<String>>>,
    policies: Option<BTreeSet<BTreeSet<String>>>,
}

impl AuthRequirementsOnElement {
    fn satisfies(&self, other: AuthRequirementsOnElement) -> bool {
        let authenticated_satisfied = self.is_authenticated || !other.is_authenticated;
        let scopes_satisfied =
            AuthRequirementsOnElement::is_implication(self.scopes.clone(), other.scopes);
        let policies_satisfied =
            AuthRequirementsOnElement::is_implication(self.policies.clone(), other.policies);
        authenticated_satisfied && scopes_satisfied && policies_satisfied
    }

    // Whether the left DNF expression materially implies the right one.
    // See: https://en.wikipedia.org/wiki/Material_conditional
    fn is_implication(
        first: Option<BTreeSet<BTreeSet<String>>>,
        second: Option<BTreeSet<BTreeSet<String>>>,
    ) -> bool {
        // Normally for DNF, you'd consider [] to be always false and [[]] to be always true,
        // and code that uses any()/all() needs no special-casing to work with these
        // definitions. However, router special-cases [] to also mean true, and so if we're
        // about to do any evaluation on DNFs, we need to do these conversions beforehand.
        let first_normalized = first.unwrap_or_else(|| EMPTY_DNF_SET.clone());
        let second_normalized = second.unwrap_or_else(|| EMPTY_DNF_SET.clone());

        // outer elements follow OR rules so we need all conditions to match as we don't know which one will be provided at runtime
        first_normalized.iter().all(|first_inner| {
            second_normalized.iter().any(|second_inner| {
                // inner elements follow AND rules which means that
                // ALL elements from second_inner has to be present in the first_inner
                first_inner.is_superset(second_inner)
            })
        })
    }
}
