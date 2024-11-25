use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::schema::FederationSchema;
use crate::schema::ValidFederationSchema;

pub(super) struct FederationSubgraph {
    pub(super) name: String,
    pub(super) url: String,
    pub(super) schema: FederationSchema,
}

pub(super) struct FederationSubgraphs {
    pub(super) subgraphs: BTreeMap<String, FederationSubgraph>,
}

impl FederationSubgraphs {
    pub(super) fn new() -> Self {
        FederationSubgraphs {
            subgraphs: BTreeMap::new(),
        }
    }

    pub(super) fn add(&mut self, subgraph: FederationSubgraph) -> Result<(), FederationError> {
        if self.subgraphs.contains_key(&subgraph.name) {
            return Err(SingleFederationError::InvalidFederationSupergraph {
                message: format!("A subgraph named \"{}\" already exists", subgraph.name),
            }
            .into());
        }
        self.subgraphs.insert(subgraph.name.clone(), subgraph);
        Ok(())
    }

    pub(super) fn get_mut(&mut self, name: &str) -> Option<&mut FederationSubgraph> {
        self.subgraphs.get_mut(name)
    }
}

impl IntoIterator for FederationSubgraphs {
    type Item = <BTreeMap<String, FederationSubgraph> as IntoIterator>::Item;
    type IntoIter = <BTreeMap<String, FederationSubgraph> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.subgraphs.into_iter()
    }
}

// TODO(@goto-bus-stop): consider an appropriate name for this in the public API
// TODO(@goto-bus-stop): should this exist separately from the `crate::subgraph::Subgraph` type?
#[derive(Debug, Clone)]
pub struct ValidFederationSubgraph {
    pub name: String,
    pub url: String,
    pub schema: ValidFederationSchema,
}

pub struct ValidFederationSubgraphs {
    pub(super) subgraphs: BTreeMap<Arc<str>, ValidFederationSubgraph>,
}

impl fmt::Debug for ValidFederationSubgraphs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ValidFederationSubgraphs ")?;
        f.debug_map().entries(self.subgraphs.iter()).finish()
    }
}

impl ValidFederationSubgraphs {
    pub(crate) fn new() -> Self {
        ValidFederationSubgraphs {
            subgraphs: BTreeMap::new(),
        }
    }

    pub(crate) fn add(&mut self, subgraph: ValidFederationSubgraph) -> Result<(), FederationError> {
        if self.subgraphs.contains_key(subgraph.name.as_str()) {
            return Err(SingleFederationError::InvalidFederationSupergraph {
                message: format!("A subgraph named \"{}\" already exists", subgraph.name),
            }
            .into());
        }
        self.subgraphs
            .insert(subgraph.name.as_str().into(), subgraph);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&ValidFederationSubgraph> {
        self.subgraphs.get(name)
    }
}

impl IntoIterator for ValidFederationSubgraphs {
    type Item = <BTreeMap<Arc<str>, ValidFederationSubgraph> as IntoIterator>::Item;
    type IntoIter = <BTreeMap<Arc<str>, ValidFederationSubgraph> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.subgraphs.into_iter()
    }
}
