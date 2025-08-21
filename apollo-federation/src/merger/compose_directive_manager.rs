use std::collections::HashMap;
use std::collections::HashSet;

use apollo_compiler::Name;
use apollo_compiler::ast::Directive;
use multimap::MultiMap;

use crate::link::Link;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;

use super::error_reporter::ErrorReporter;

const DISALLOWED_IDENTITIES: [&str; 11] = [
    "https://specs.apollo.dev/core",
    "https://specs.apollo.dev/join",
    "https://specs.apollo.dev/link",
    "https://specs.apollo.dev/tag",
    "https://specs.apollo.dev/inaccessible",
    "https://specs.apollo.dev/federation",
    "https://specs.apollo.dev/authenticated",
    "https://specs.apollo.dev/requiresScopes",
    "https://specs.apollo.dev/source",
    "https://specs.apollo.dev/context",
    "https://specs.apollo.dev/cost",
];

pub(crate) struct ComposeDirectiveManager {
    merge_directive_map: HashMap<Name, HashSet<Name>>,
}

struct MergeDirectiveItem {
    subgraph_name: String,
    feature: Link,
    directive_name: Name,
    directive_name_as: Name,
    compose_directive: Directive,
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
