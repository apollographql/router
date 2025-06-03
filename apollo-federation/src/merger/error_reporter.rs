use crate::error::{CompositionError, FederationError};
use crate::subgraph::SubgraphError;
use crate::supergraph::CompositionHint;

pub(crate) struct ErrorReporter {
    errors: Vec<CompositionError>,
    hints: Vec<CompositionHint>,
}

impl ErrorReporter {
    pub(crate) fn new() -> Self {
        Self {
            errors: Vec::new(),
            hints: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn add_subgraph_error(&mut self, name: &str, error: impl Into<FederationError>) {
        let error = error.into();
        let error = SubgraphError {
            subgraph: name.into(),
            error,
        };
        self.errors.push(error.into());
    }

    #[allow(dead_code)]
    pub(crate) fn add_error(&mut self, error: CompositionError) {
        self.errors.push(error);
    }

    #[allow(dead_code)]
    pub(crate) fn add_hint(&mut self, hint: CompositionHint) {
        self.hints.push(hint);
    }

    pub(crate) fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    pub(crate) fn into_errors_and_hints(self) -> (Vec<CompositionError>, Vec<CompositionHint>) {
        (self.errors, self.hints)
    }
}
