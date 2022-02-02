use crate::prelude::graphql::*;
use std::sync::Arc;

#[derive(Clone)]
pub struct Context<T = Arc<http::Request<Request>>> {
    /// Original request to the Router.
    pub request: T,

    // Allows adding custom extensions to the context.
    extensions: Object,
}

impl Context<()> {
    pub fn new() -> Self {
        Context {
            request: (),
            extensions: Default::default(),
        }
    }

    pub(crate) fn with_request<T>(self, request: T) -> Context<T> {
        // TODO this could be improved with this RFC https://github.com/rust-lang/rust/issues/86555
        let Self {
            request: _,
            extensions,
        } = self;
        Context {
            request,
            extensions,
        }
    }
}

impl<T> Context<T> {
    pub fn extensions(&self) -> &Object {
        &self.extensions
    }

    pub fn extensions_mut(&mut self) -> &mut Object {
        &mut self.extensions
    }
}

impl Default for Context<()> {
    fn default() -> Self {
        Self::new()
    }
}
