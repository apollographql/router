use std::collections::HashMap;
use std::collections::HashSet;

use apollo_compiler::Name;

pub(crate) struct ComposeDirectiveManager {
    merge_directive_map: HashMap<Name, HashSet<Name>>,
}

impl ComposeDirectiveManager {
    pub(crate) fn new() -> Self {
        Self {
            merge_directive_map: HashMap::new(),
        }
    }

    pub(crate) fn should_compose_directive(
        &self,
        subgraph_name: &str,
        directive_name: &Name,
    ) -> bool {
        self.merge_directive_map
            .get(subgraph_name)
            .is_some_and(|set| set.contains(directive_name))
    }
}
