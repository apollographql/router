use apollo_compiler::collections::HashMap;
use apollo_compiler::Name;

use super::TypeDefinitionPosition;
use crate::error::FederationError;
use crate::schema::SubgraphMetadata;
use crate::ValidFederationSchema;
use crate::ValidFederationSubgraph;
use crate::ValidFederationSubgraphs;

#[derive(Clone, Debug)]
struct SchemaUpgrader<'a> {
    schema: ValidFederationSchema,
    name: String,
    url: String,
    subgraphs: &'a ValidFederationSubgraphs,
    object_type_map: &'a HashMap<Name, HashMap<String, TypeInfo>>,
}

#[derive(Clone, Debug)]
struct TypeInfo {
    pos: TypeDefinitionPosition,
    metadata: SubgraphMetadata,
}

pub(crate) fn upgrade_subgraphs_if_necessary(
    subgraphs: ValidFederationSubgraphs,
) -> Result<(), FederationError> {
    let mut federation_subgraphs = ValidFederationSubgraphs::new();

    // if all subgraphs are fed 2, there is no upgrade to be done
    if subgraphs
        .subgraphs
        .values()
        .all(|subgraph| subgraph.schema.is_fed_2())
    {
        return Ok(());
    }

    let mut object_type_map: HashMap<Name, HashMap<String, TypeInfo>> = Default::default();
    for subgraph in subgraphs.subgraphs.values() {
        if let Some(subgraph_metadata) = subgraph.schema.subgraph_metadata() {
            for pos in subgraph.schema.get_types() {
                if matches!(pos, TypeDefinitionPosition::Object(_) | TypeDefinitionPosition::Interface(_)) {
                    object_type_map
                    .entry(pos.type_name().clone())
                    .or_default()
                    .insert(
                        subgraph.name.clone(),
                        TypeInfo {
                            pos: pos.clone(),
                            metadata: subgraph_metadata.clone(),
                        },
                    );                 
                }
            }
        }
    }
    for (_name, subgraph) in subgraphs.subgraphs.iter() {
        if subgraph.schema.is_fed_2() {
            federation_subgraphs.add(ValidFederationSubgraph {
                name: subgraph.name.clone(),
                url: subgraph.url.clone(),
                schema: subgraph.schema.clone(),
            })?;
        } else {
            let upgrader = SchemaUpgrader::new(subgraph, &subgraphs, &object_type_map);
            federation_subgraphs.add(upgrader.upgrade()?)?;
        }
    }
    // TODO: Return federation_subgraphs
    todo!();
}

impl<'a> SchemaUpgrader<'a> {
    fn new(
        original_subgraph: &ValidFederationSubgraph,
        subgraphs: &'a ValidFederationSubgraphs,
        object_type_map: &'a HashMap<Name, HashMap<String, TypeInfo>>,
    ) -> Self {
        SchemaUpgrader {
            schema: original_subgraph.schema.clone(),
            name: original_subgraph.name.clone(),
            url: original_subgraph.url.clone(),
            subgraphs,
            object_type_map,
        }
    }

    fn upgrade(&self) -> Result<ValidFederationSubgraph, FederationError> {
        self.pre_upgrade_validations();

        self.fix_federation_directives_arguments();

        self.remove_external_on_interface();

        self.remove_external_on_object_types();

        // Note that we remove all external on type extensions first, so we don't have to care about it later in @key, @provides and @requires.
        self.remove_external_on_type_extensions();

        self.fix_inactive_provides_and_requires();

        self.remove_type_extensions();

        self.remove_directives_on_interface();

        // Note that this rule rely on being after `removeDirectivesOnInterface` in practice (in that it doesn't check interfaces).
        self.remove_provides_on_non_composite();

        // Note that this should come _after_ all the other changes that may remove/update federation directives, since those may create unused
        // externals. Which is why this is toward  the end.
        self.remove_unused_externals();

        self.add_shareable();

        self.remove_tag_on_external();

        todo!();
    }

    fn pre_upgrade_validations(&self) {
        todo!();
    }

    fn fix_federation_directives_arguments(&self) {
        todo!();
    }

    fn remove_external_on_interface(&self) {
        todo!();
    }

    fn remove_external_on_object_types(&self) {
        todo!();
    }

    fn remove_external_on_type_extensions(&self) {
        todo!();
    }

    fn fix_inactive_provides_and_requires(&self) {
        todo!();
    }

    fn remove_type_extensions(&self) {
        todo!();
    }

    fn remove_directives_on_interface(&self) {
        todo!();
    }

    fn remove_provides_on_non_composite(&self) {
        todo!();
    }

    fn remove_unused_externals(&self) {
        todo!();
    }

    fn add_shareable(&self) {
        todo!();
    }

    fn remove_tag_on_external(&self) {
        todo!();
    }
}
