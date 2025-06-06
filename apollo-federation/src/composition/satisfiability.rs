mod satisfiability_error;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;

use crate::CONTEXT_VERSIONS;
use crate::JoinSpecDefinition;
use crate::bail;
use crate::error::CompositionError;
use crate::error::FederationError;
use crate::link::spec::Identity;
use crate::schema::FederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;
use crate::supergraph::Merged;
use crate::supergraph::Satisfiable;
use crate::supergraph::Supergraph;
use crate::validate_supergraph_for_query_planning;

pub fn validate_satisfiability(
    _supergraph: Supergraph<Merged>,
) -> Result<Supergraph<Satisfiable>, Vec<CompositionError>> {
    panic!("validate_satisfiability is not implemented yet")
}

struct ValidationContext {
    supergraph_schema: FederationSchema,
    join_spec: &'static JoinSpecDefinition,
    join_type_directive: Node<ast::DirectiveDefinition>,
    join_field_directive: Node<ast::DirectiveDefinition>,
    types_to_contexts: IndexMap<Name, IndexSet<String>>, // mapping from type name to context names
}

impl ValidationContext {
    #[allow(unused)]
    pub(crate) fn new(supergraph: &Supergraph<Merged>) -> Result<Self, FederationError> {
        // TODO: Avoid this clone by holding `FederationSchema` directly in `Merged` struct.
        let supergraph_schema =
            FederationSchema::new(supergraph.state.schema().clone().into_inner())?;
        let (_, join_spec, _) = validate_supergraph_for_query_planning(&supergraph_schema)?;
        let join_type_directive = join_spec
            .type_directive_definition(&supergraph_schema)?
            .clone();
        let join_field_directive = join_spec
            .field_directive_definition(&supergraph_schema)?
            .clone();

        let mut types_to_contexts = IndexMap::default();
        let Some(links_metadata) = supergraph_schema.metadata() else {
            bail!("Supergraph schema should have links metadata");
        };
        if let Some(context_link) = links_metadata.for_identity(&Identity::context_identity()) {
            let Some(context_spec) = CONTEXT_VERSIONS.find(&context_link.url.version) else {
                bail!(
                    "Unexpected context spec version {}",
                    context_link.url.version
                );
            };
            let context_directive =
                context_spec.context_directive_definition(&supergraph_schema)?;
            for app in supergraph_schema.context_directive_applications()? {
                let Ok(app) = app else {
                    continue;
                };
                let args = app.arguments();
                let target_type = supergraph_schema.get_type(app.target().type_name().clone())?;
                let mut type_names = vec![target_type.type_name().clone()];
                match target_type {
                    TypeDefinitionPosition::Interface(interface_type) => {
                        type_names.extend(
                            supergraph_schema
                                .possible_runtime_types(interface_type.clone().into())?
                                .into_iter()
                                .map(|type_pos| type_pos.type_name.clone()),
                        );
                    }
                    TypeDefinitionPosition::Union(union_type) => {
                        let union_def = union_type.get(supergraph_schema.schema())?;
                        type_names.extend(union_def.members.iter().map(|m| m.name.clone()));
                    }
                    _ => {}
                };
                for type_name in type_names {
                    types_to_contexts
                        .entry(type_name)
                        .or_insert_with(IndexSet::default)
                        .insert(args.name.to_string());
                }
            }
        }

        Ok(ValidationContext {
            supergraph_schema,
            join_spec,
            join_type_directive,
            join_field_directive,
            types_to_contexts,
        })
    }

    #[allow(unused)]
    pub(crate) fn is_shareable(
        &self,
        field: &FieldDefinitionPosition,
    ) -> Result<bool, FederationError> {
        let Ok(type_in_supergraph) = self
            .supergraph_schema
            .get_type(field.parent().type_name().clone())
        else {
            bail!("Type {} should exist in the supergraph", field.parent());
        };
        let Ok(type_in_supergraph) = CompositeTypeDefinitionPosition::try_from(type_in_supergraph)
        else {
            bail!("Type {} should be composite", field.parent().type_name());
        };
        if !type_in_supergraph.is_object_type() {
            return Ok(false);
        }

        let Ok(field_in_supergraph) = type_in_supergraph.field(field.field_name().clone()) else {
            bail!(
                "Field {} should exist in the supergraph",
                field.field_name()
            );
        };
        let join_field_apps = field_in_supergraph
            .get_applied_directives(&self.supergraph_schema, &self.join_field_directive.name);
        // A field is shareable if either:
        // 1) there is not join__field, but multiple join__type
        // 2) there is more than one join__field where the field is neither external nor overridden.
        if join_field_apps.is_empty() {
            let join_type_apps = type_in_supergraph
                .get_applied_directives(&self.supergraph_schema, &self.join_type_directive.name);
            Ok(join_type_apps.len() > 1)
        } else {
            let count = join_field_apps
                .iter()
                .filter(|app| {
                    self.join_spec
                        .field_directive_arguments(app)
                        .is_ok_and(|args| {
                            !(args.external.is_some_and(|x| x))
                                && !(args.user_overridden.is_some_and(|x| x))
                        })
                })
                .count();
            Ok(count > 1)
        }
    }

    #[allow(unused)]
    fn matching_contexts(&self, type_name: &Name) -> Option<&IndexSet<String>> {
        self.types_to_contexts.get(type_name)
    }
}
