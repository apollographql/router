use apollo_compiler::{collections::HashMap, Name};

use crate::{
    error::FederationError, schema::SubgraphMetadata, ValidFederationSchema, ValidFederationSubgraph, ValidFederationSubgraphs
};

use super::TypeDefinitionPosition;

#[derive(Clone, Debug)]
struct SchemaUpgrader {
    schema: ValidFederationSchema,
    subgraph: ValidFederationSubgraph,
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
        .iter()
        .all(|(_name, subgraph)| subgraph.schema.is_fed_2())
    {
        return Ok(());
    }

    let mut object_type_map: HashMap<Name, HashMap<String, TypeInfo>> = Default::default();
    for (_, subgraph) in subgraphs.subgraphs.iter() {
        if let Some(subgraph_metadata) = subgraph.schema.subgraph_metadata() {
            for pos in subgraph.schema.get_types() {
                match pos {
                    TypeDefinitionPosition::Object(_) | TypeDefinitionPosition::Interface(_) => {
                        object_type_map
                            .entry(pos.type_name().clone())
                            .or_insert_with(HashMap::default)
                            .insert(subgraph.name.clone(), TypeInfo { pos: pos.clone(), metadata: subgraph_metadata.clone() });
                    }
                    _ => {
                        // ignore
                    }
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

impl SchemaUpgrader {
    fn new(
        original_subgraph: &ValidFederationSubgraph,
        _subgraphs: &ValidFederationSubgraphs,
        _object_type_map: &HashMap<Name, HashMap<String, TypeInfo>>,
    ) -> Self {
        SchemaUpgrader {
            schema: original_subgraph.schema.clone(),
            subgraph: original_subgraph.clone(),
        }
    }
    
    fn upgrade(&self) -> Result<ValidFederationSubgraph, FederationError> {
        todo!();
    }
}
