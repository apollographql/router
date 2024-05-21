use std::collections::BTreeMap;
use std::fmt::Formatter;
use std::sync::Arc;

use apollo_compiler::name;
use apollo_compiler::schema::ComponentName;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::validation::Valid;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use indexmap::map::Entry;
use indexmap::IndexMap;
use indexmap::IndexSet;

use crate::error::FederationError;
use crate::link::spec::Identity;
use crate::link::Link;
use crate::link::LinkError;
use crate::link::DEFAULT_LINK_NAME;
use crate::subgraph::spec::AppliedFederationLink;
use crate::subgraph::spec::FederationSpecDefinitions;
use crate::subgraph::spec::LinkSpecDefinitions;
use crate::subgraph::spec::ANY_SCALAR_NAME;
use crate::subgraph::spec::ENTITIES_QUERY;
use crate::subgraph::spec::ENTITY_UNION_NAME;
use crate::subgraph::spec::FEDERATION_V2_DIRECTIVE_NAMES;
use crate::subgraph::spec::KEY_DIRECTIVE_NAME;
use crate::subgraph::spec::SERVICE_SDL_QUERY;
use crate::subgraph::spec::SERVICE_TYPE;

mod database;
pub mod spec;

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
                    .push(defaults.applied_link_directive().into());
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
                    .push(defaults.applied_link_directive().into());
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
        let fieldset_scalar_name = &fed_definitions.fieldset_scalar_name;
        schema
            .types
            .entry(fieldset_scalar_name.clone())
            .or_insert_with(|| {
                fed_definitions
                    .fieldset_scalar_definition(fieldset_scalar_name.clone())
                    .into()
            });

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
                fields: IndexMap::new(),
                implements_interfaces: IndexSet::new(),
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

pub struct Subgraphs {
    subgraphs: BTreeMap<String, Arc<Subgraph>>,
}

#[allow(clippy::new_without_default)]
impl Subgraphs {
    pub fn new() -> Self {
        Subgraphs {
            subgraphs: BTreeMap::new(),
        }
    }

    pub fn add(&mut self, subgraph: Subgraph) -> Result<(), String> {
        if self.subgraphs.contains_key(&subgraph.name) {
            return Err(format!("A subgraph named {} already exists", subgraph.name));
        }
        self.subgraphs
            .insert(subgraph.name.clone(), Arc::new(subgraph));
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<Arc<Subgraph>> {
        self.subgraphs.get(name).cloned()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subgraph::database::keys;

    #[test]
    fn can_inspect_a_type_key() {
        // TODO: no schema expansion currently, so need to having the `@link` to `link` and the
        // @link directive definition for @link-bootstrapping to work. Also, we should
        // theoretically have the @key directive definition added too (but validation is not
        // wired up yet, so we get away without). Point being, this is just some toy code at
        // the moment.

        let schema = r#"
          extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0", import: ["Import"])
            @link(url: "https://specs.apollo.dev/federation/v2.3", import: ["@key"])

          type Query {
            t: T
          }

          type T @key(fields: "id") {
            id: ID!
            x: Int
          }

          enum link__Purpose {
            SECURITY
            EXECUTION
          }

          scalar Import

          directive @link(url: String, as: String, import: [Import], for: link__Purpose) repeatable on SCHEMA
        "#;

        let subgraph = Subgraph::new("S1", "http://s1", schema).unwrap();
        let keys = keys(&subgraph.schema, &name!("T"));
        assert_eq!(keys.len(), 1);
        assert_eq!(keys.first().unwrap().type_name, name!("T"));

        // TODO: no accessible selection yet.
    }
}
