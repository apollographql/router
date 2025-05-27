use crate::error::SingleFederationError;
use crate::supergraph::CompositionHint;

pub(crate) struct ErrorReporter {
    errors: Vec<SingleFederationError>,
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
    pub(crate) fn add_error(&mut self, error: SingleFederationError) {
        self.errors.push(error);
    }

    #[allow(dead_code)]
    pub(crate) fn add_hint(&mut self, hint: CompositionHint) {
        self.hints.push(hint);
    }

    pub(crate) fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    pub(crate) fn into_errors_and_hints(
        self,
    ) -> (Vec<SingleFederationError>, Vec<CompositionHint>) {
        (self.errors, self.hints)
    }
}
