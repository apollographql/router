//! Layer 2: drive `apollo_federation::composition::compose` over a set of
//! generated subgraph SDLs.
//!
//! Composition is run only on the HEAD side. The resulting supergraph SDL is
//! the contract handed to both planners; we don't need to compose twice.

use apollo_federation::composition::{CompositionOptions, compose};
use apollo_federation::error::CompositionError;
use apollo_federation::subgraph::typestate::{Initial, Subgraph};

use crate::subgraph_gen::SubgraphSdl;

#[derive(Debug)]
pub enum ComposeOutcome {
    /// Composition succeeded; `supergraph_sdl` can be fed to either planner.
    Composed { supergraph_sdl: String },
    /// One or more `Subgraph::parse` calls failed before composition.
    ParseFailed { errors: Vec<String> },
    /// Composition itself reported errors. These can be legitimate
    /// (generator produced an unsatisfiable graph) or surprising (planner bug
    /// on what should compose). Caller decides how to classify.
    CompositionFailed { errors: Vec<CompositionError> },
}

pub fn try_compose(subgraphs: &[SubgraphSdl]) -> ComposeOutcome {
    let mut parsed: Vec<Subgraph<Initial>> = Vec::with_capacity(subgraphs.len());
    let mut parse_errors: Vec<String> = Vec::new();

    for s in subgraphs {
        let url = format!("http://{}", s.name);
        match Subgraph::parse(&s.name, &url, &s.sdl) {
            Ok(sg) => match sg.into_fed2_test_subgraph(true) {
                Ok(fed2) => parsed.push(fed2),
                Err(e) => parse_errors.push(format!("{}: {}", s.name, e)),
            },
            Err(e) => parse_errors.push(format!("{}: {}", s.name, e)),
        }
    }

    if !parse_errors.is_empty() {
        return ComposeOutcome::ParseFailed { errors: parse_errors };
    }

    match compose(parsed, CompositionOptions::default()) {
        Ok(supergraph) => {
            let supergraph_sdl = supergraph.schema().schema().to_string();
            ComposeOutcome::Composed { supergraph_sdl }
        }
        Err(errors) => ComposeOutcome::CompositionFailed { errors },
    }
}
