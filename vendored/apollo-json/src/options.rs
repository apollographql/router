/// Default container nesting limit.
pub(crate) const DEFAULT_MAX_DEPTH: usize = 128;

/// Resource limits applied while parsing.
///
/// Both limits turn adversarial inputs into hard parse errors instead of
/// unbounded memory growth or deep stacks.
#[derive(Clone, Debug)]
pub struct ParseOptions {
    pub(crate) max_arena_bytes: usize,
    pub(crate) max_depth: usize,
}

impl Default for ParseOptions {
    /// 256 MiB arena limit, 128 levels of container nesting.
    fn default() -> Self {
        ParseOptions {
            max_arena_bytes: 256 * 1024 * 1024,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }
}

impl ParseOptions {
    /// Caps the total bytes the parsed document may retain (input copy,
    /// nodes, and container storage). Exceeding it aborts the parse with
    /// [`JsonError::ArenaLimitExceeded`](crate::JsonError::ArenaLimitExceeded).
    #[must_use]
    pub fn with_max_arena_bytes(mut self, limit: usize) -> Self {
        self.max_arena_bytes = limit;
        self
    }

    /// Caps container nesting depth. Exceeding it aborts the parse with
    /// [`JsonError::DepthLimitExceeded`](crate::JsonError::DepthLimitExceeded).
    #[must_use]
    pub fn with_max_depth(mut self, limit: usize) -> Self {
        self.max_depth = limit;
        self
    }
}
